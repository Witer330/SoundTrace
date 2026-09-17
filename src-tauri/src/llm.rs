use crate::db::Db;
use crate::error::{err, AppResult};
use rusqlite::params;
use serde_json::json;
use std::io::Read;
use tauri::Emitter;

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeDone {
    pub recording_id: i64,
    pub ok: bool,
    pub error: String,
}

/// OpenAI 兼容 Chat Completions 调用（阻塞）
fn chat(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[(String, String)],
    max_tokens: u32,
) -> AppResult<String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let msgs: Vec<_> = messages
        .iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect();
    let body = json!({
        "model": model,
        "messages": msgs,
        "temperature": 0.3,
        "max_tokens": max_tokens,
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| err(format!("创建 HTTP 客户端失败: {e}")))?;
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .map_err(|e| err(format!("请求失败: {e}")))?;

    let status = resp.status();
    let mut raw = String::new();
    resp.take(4 * 1024 * 1024).read_to_string(&mut raw)?;
    if !status.is_success() {
        return Err(err(format!("API 返回 {status}: {}", raw.chars().take(400).collect::<String>())));
    }
    let parsed: serde_json::Value = serde_json::from_str(&raw)?;
    let content = parsed
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| err(format!("API 响应格式异常: {}", raw.chars().take(300).collect::<String>())))?;
    Ok(content.to_string())
}

/// 设置测试：发一条极短消息验证连通性
pub fn test_llm(base_url: &str, api_key: &str, model: &str) -> AppResult<String> {
    let reply = chat(
        base_url,
        api_key,
        model,
        &[("user".into(), "回复“连接成功”四个字".into())],
        20,
    )?;
    Ok(reply)
}

const FINAL_PROMPT: &str = r#"你是一名专业的会议记录员。请根据以下会议转写稿，输出一份结构化中文 Markdown 会议纪要，必须包含以下章节：

## 一、会议概要
3~5 句话概括会议主题、背景与整体结论。

## 二、关键信息与决议
逐条列出会上达成的决议、重要数据和结论。没有则写“无”。

## 三、行动项
用 Markdown 表格列出：| 事项 | 负责人 | 时限 |。负责人未提及写“待定”。没有则写“无”。

## 四、待澄清问题
列出转写稿中语义不明或前后矛盾、需要后续确认的点。没有则写“无”。

要求：只依据转写稿内容，不要编造；保留具体数字、日期、人名；语言简练。"#;

const PARTIAL_PROMPT: &str = r#"你是一名专业的会议记录员。以下是一段长会议转写稿的其中一个片段（带时间戳）。请输出该片段的要点小结（Markdown 列表），保留关键数字、决议、行动项与时间点，不要编造。"#;

const MERGE_PROMPT: &str = r#"你是一名专业的会议记录员。以下是一份长会议转写稿按时间顺序分段生成的要点小结。请把它们合并成一份完整的会议纪要，必须包含章节：一、会议概要；二、关键信息与决议；三、行动项（Markdown 表格：事项/负责人/时限）；四、待澄清问题。去重合并，不要编造。"#;

fn fmt_ts(ms: i64) -> String {
    let s = ms / 1000;
    format!("[{:02}:{:02}:{:02}]", s / 3600, (s / 60) % 60, s % 60)
}

/// 生成会议纪要（后台线程），完成发 summarize://done 事件
pub fn summarize(db: std::sync::Arc<Db>, app: tauri::AppHandle, recording_id: i64) {
    std::thread::spawn(move || {
        let result = (|| -> AppResult<String> {
            let (base_url, api_key, model, title, recorded_at, participants, texts) = {
                let conn = db.conn.lock().unwrap();
                let config: (String, String, String) = (
                    conn.query_row("SELECT value FROM settings WHERE key='llm.baseUrl'", [], |r| r.get(0))
                        .unwrap_or_default(),
                    conn.query_row("SELECT value FROM settings WHERE key='llm.apiKey'", [], |r| r.get(0))
                        .unwrap_or_default(),
                    conn.query_row("SELECT value FROM settings WHERE key='llm.model'", [], |r| r.get(0))
                        .unwrap_or_default(),
                );
                let rec: (String, Option<String>, String) = conn
                    .query_row(
                        "SELECT title, recorded_at, participants FROM recordings WHERE id=?1",
                        [recording_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|_| err("录音不存在"))?;
                let mut stmt = conn.prepare(
                    "SELECT start_ms, text FROM segments WHERE recording_id=?1 ORDER BY start_ms",
                )?;
                let texts: Vec<String> = stmt
                    .query_map([recording_id], |r| {
                        Ok(format!("{} {}", fmt_ts(r.get::<_, i64>(0)?), r.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                (config.0, config.1, config.2, rec.0, rec.1, rec.2, texts)
            };

            if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
                return Err(err("请先在设置中配置 LLM（Base URL / API Key / 模型）"));
            }
            if texts.is_empty() {
                return Err(err("尚无转写内容，请先完成转写"));
            }

            let header = format!(
                "会议主题：{title}\n日期：{}\n参会人：{}\n\n",
                recorded_at.as_deref().map(|s| &s[..s.len().min(10)]).unwrap_or("未知"),
                if participants.is_empty() { "未记录" } else { &participants },
            );

            let full = texts.join("\n");
            let summary = if full.chars().count() <= 30000 {
                chat(
                    &base_url,
                    &api_key,
                    &model,
                    &[
                        ("system".into(), FINAL_PROMPT.into()),
                        ("user".into(), format!("{header}会议转写稿：\n\n{full}")),
                    ],
                    8000,
                )?
            } else {
                // 长转写：分段小结 → 合并
                let mut partials: Vec<String> = Vec::new();
                let mut buf = String::new();
                for line in &texts {
                    if buf.chars().count() + line.chars().count() > 25000 {
                        partials.push(
                            chat(
                                &base_url,
                                &api_key,
                                &model,
                                &[
                                    ("system".into(), PARTIAL_PROMPT.into()),
                                    ("user".into(), format!("{header}片段内容：\n\n{buf}")),
                                ],
                                4000,
                            )?,
                        );
                        buf.clear();
                    }
                    buf.push_str(line);
                    buf.push('\n');
                }
                if !buf.is_empty() {
                    partials.push(
                        chat(
                            &base_url,
                            &api_key,
                            &model,
                            &[
                                ("system".into(), PARTIAL_PROMPT.into()),
                                ("user".into(), format!("{header}片段内容：\n\n{buf}")),
                            ],
                            4000,
                        )?,
                    );
                }
                chat(
                    &base_url,
                    &api_key,
                    &model,
                    &[
                        ("system".into(), MERGE_PROMPT.into()),
                        ("user".into(), format!("{header}各片段要点：\n\n{}", partials.join("\n\n---\n\n"))),
                    ],
                    8000,
                )?
            };

            // 保存
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE recordings SET summary_md=?1, llm_model=?2, updated_at=?3 WHERE id=?4",
                params![summary, model, now, recording_id],
            )?;
            Ok(summary)
        })();

        let _ = app.emit(
            "summarize://done",
            SummarizeDone {
                recording_id,
                ok: result.is_ok(),
                error: result.err().map(|e| e.msg).unwrap_or_default(),
            },
        );
    });
}
