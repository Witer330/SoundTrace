use crate::error::AppResult;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

pub struct Db {
    pub conn: Mutex<Connection>,
}

impl Db {
    pub fn open(root: &Path) -> AppResult<Self> {
        let conn = Connection::open(root.join("soundtrace.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&conn)?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let v = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })?;
        Ok(v)
    }

    pub fn set_settings(&self, entries: &HashMap<String, String>) -> AppResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value")?;
            for (k, v) in entries {
                stmt.execute((k, v))?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn all_settings(&self) -> AppResult<HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
        let map = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(map)
    }
}

fn migrate(conn: &Connection) -> AppResult<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(SCHEMA_V1)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(())
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS settings(
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS recordings(
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  file_hash    TEXT NOT NULL UNIQUE,
  file_path    TEXT NOT NULL,
  orig_name    TEXT NOT NULL,
  title        TEXT NOT NULL,
  recorded_at  TEXT,
  duration_sec REAL NOT NULL DEFAULT 0,
  size_bytes   INTEGER NOT NULL DEFAULT 0,
  format       TEXT NOT NULL DEFAULT '',
  status       TEXT NOT NULL DEFAULT 'imported',  -- imported|queued|transcribing|done|failed
  notes        TEXT NOT NULL DEFAULT '',
  participants TEXT NOT NULL DEFAULT '',
  summary_md   TEXT,
  llm_model    TEXT,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS segments(
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
  start_ms     INTEGER NOT NULL,
  end_ms       INTEGER NOT NULL,
  text         TEXT NOT NULL,
  speaker      INTEGER
);
CREATE INDEX IF NOT EXISTS idx_segments_rec ON segments(recording_id, start_ms);

CREATE TABLE IF NOT EXISTS tags(
  id   INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS recording_tags(
  recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
  tag_id       INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  PRIMARY KEY(recording_id, tag_id)
);

CREATE TABLE IF NOT EXISTS bookmarks(
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
  time_ms      INTEGER NOT NULL,
  label        TEXT NOT NULL DEFAULT '',
  note         TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_bookmarks_rec ON bookmarks(recording_id, time_ms);

CREATE TABLE IF NOT EXISTS jobs(
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  recording_id INTEGER REFERENCES recordings(id) ON DELETE CASCADE,
  kind         TEXT NOT NULL,             -- transcribe|summarize|peaks
  status       TEXT NOT NULL DEFAULT 'pending', -- pending|running|done|failed|canceled
  progress     REAL NOT NULL DEFAULT 0,
  error        TEXT,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
"#;
