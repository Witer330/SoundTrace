import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { errMsg, importPaths, isTauri, searchAll, type SearchResults } from "../api";
import { useAppStore } from "../store";
import {
  formatBytes,
  formatDate,
  formatDateTime,
  formatDuration,
  formatMsTime,
  STATUS_LABEL,
  type ImportResult,
  type Recording,
  type RecordingStatus,
} from "../types";

function StatusBadge({ status }: { status: RecordingStatus }) {
  const style: Record<RecordingStatus, string> = {
    imported: "bg-ink-700 text-mist-300",
    queued: "bg-amber-900/60 text-amber-200",
    transcribing: "bg-cyan-900/60 text-cyan-200 animate-pulse",
    done: "bg-emerald-900/60 text-emerald-200",
    failed: "bg-red-900/60 text-red-200",
  };
  return (
    <span className={`px-2 py-0.5 rounded text-xs ${style[status]}`}>
      {STATUS_LABEL[status]}
    </span>
  );
}

export default function LibraryView() {
  const recordings = useAppStore((s) => s.recordings);
  const loaded = useAppStore((s) => s.recordingsLoaded);
  const query = useAppStore((s) => s.libraryQuery);
  const setQuery = useAppStore((s) => s.setLibraryQuery);
  const go = useAppStore((s) => s.go);
  const refresh = useAppStore((s) => s.refreshRecordings);

  const [dragging, setDragging] = useState(false);
  const [importing, setImporting] = useState(false);
  const [lastResult, setLastResult] = useState<ImportResult | null>(null);
  const [searchResults, setSearchResults] = useState<SearchResults | null>(null);
  const dragDepth = useRef(0);

  // 全局搜索（防抖 350ms；清空回到列表）
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setSearchResults(null);
      return;
    }
    const timer = setTimeout(() => {
      if (!isTauri()) return;
      searchAll(q)
        .then(setSearchResults)
        .catch((e) => setErrorSearch(errMsg(e)));
    }, 350);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query]);

  function setErrorSearch(msg: string) {
    setLastResult({ imported: [], skipped: [{ path: "", reason: msg }] });
  }

  async function doImport(paths: string[] | null) {
    if (!paths || paths.length === 0) return;
    setImporting(true);
    try {
      const result = await importPaths(paths);
      setLastResult(result);
      await refresh();
    } catch (e) {
      setLastResult({ imported: [], skipped: [{ path: "", reason: errMsg(e) }] });
    } finally {
      setImporting(false);
    }
  }

  async function pickFiles() {
    const paths = await open({
      multiple: true,
      filters: [{ name: "音频", extensions: ["m4a", "mp3", "wav", "amr", "3gp", "wma", "aac", "flac", "ogg", "opus"] }],
    });
    await doImport(Array.isArray(paths) ? paths : paths ? [paths] : null);
  }

  async function pickFolder() {
    const dir = await open({ directory: true, multiple: false });
    await doImport(dir ? [dir] : null);
  }

  useEffect(() => {
    if (!isTauri()) return;
    const webview = getCurrentWebview();
    const unlisten = webview.onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "enter") {
        dragDepth.current += 1;
        setDragging(true);
      } else if (p.type === "leave") {
        dragDepth.current = Math.max(0, dragDepth.current - 1);
        if (dragDepth.current === 0) setDragging(false);
      } else if (p.type === "drop") {
        dragDepth.current = 0;
        setDragging(false);
        void doImport(p.paths);
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const q = query.trim();
  const filtered = !q
    ? recordings
    : (searchResults?.recordings ?? []).map((hit) =>
        recordings.find((r) => r.id === hit.id),
      ).filter((r): r is Recording => !!r);

  // 搜索结果：命中录音 + 命中转写片段
  const searchSection =
    q && searchResults ? (
      <div className="space-y-4">
        {searchResults.segments.length > 0 && (
          <div className="space-y-2">
            <div className="text-xs text-ink-500">
              转写片段命中 {searchResults.segments.length} 条
            </div>
            {searchResults.segments.map((seg, i) => (
              <button
                key={i}
                onClick={() => go({ kind: "detail", id: seg.recordingId, seekMs: seg.startMs })}
                className="w-full text-left px-4 py-3 rounded-xl bg-ink-900 border border-ink-800 hover:border-ink-600 transition-colors"
              >
                <div className="flex items-center gap-2 text-xs text-ink-500">
                  <span className="text-cyan-accent tabular-nums">{formatMsTime(seg.startMs)}</span>
                  <span className="truncate">{seg.recordingTitle}</span>
                  <span>·</span>
                  <span>{formatDate(seg.recordedAt)}</span>
                </div>
                <div className="text-sm text-mist-300 mt-1 line-clamp-2">{seg.snippet}</div>
              </button>
            ))}
          </div>
        )}
        {searchResults.recordings.length === 0 && searchResults.segments.length === 0 && (
          <div className="text-center text-mist-400 text-sm py-12">
            没有找到与「{q}」相关的内容
          </div>
        )}
      </div>
    ) : null;

  return (
    <div className="h-full flex flex-col relative">
      {/* 工具栏 */}
      <div className="flex items-center gap-2 px-4 h-14 border-b border-ink-800 bg-ink-900/60">
        <input
          type="text"
          className="flex-1 max-w-md"
          placeholder="搜索标题或标签…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="flex-1" />
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200 transition-colors"
          onClick={pickFiles}
          disabled={importing}
        >
          导入文件
        </button>
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium hover:opacity-90 transition-opacity"
          onClick={pickFolder}
          disabled={importing}
        >
          {importing ? "导入中…" : "导入文件夹"}
        </button>
      </div>

      {/* 导入结果提示 */}
      {lastResult && (
        <div className="mx-4 mt-3 px-3 py-2 rounded-lg bg-ink-850 border border-ink-700 text-sm flex items-start gap-2">
          <div className="flex-1 text-mist-300">
            导入完成：新增 {lastResult.imported.length} 条
            {lastResult.skipped.length > 0 && (
              <span className="text-mist-400">
                {" "}· 跳过 {lastResult.skipped.length} 条
                （{lastResult.skipped.map((s) => s.reason).join("；").slice(0, 120)}）
              </span>
            )}
          </div>
          <button className="text-ink-500 hover:text-mist-300" onClick={() => setLastResult(null)}>
            ✕
          </button>
        </div>
      )}

      {/* 列表 */}
      <div className="flex-1 overflow-y-auto p-4">
        {!loaded ? (
          <div className="text-mist-400 text-sm">加载中…</div>
        ) : searchSection ? (
          <>
            {searchSection}
            {filtered.length > 0 && (
              <div className="space-y-2 mt-4">
                <div className="text-xs text-ink-500">录音命中 {filtered.length} 条</div>
                {filtered.map((r) => (
                  <button
                    key={r.id}
                    onClick={() => go({ kind: "detail", id: r.id })}
                    className="w-full flex items-center gap-4 px-4 py-3 rounded-xl bg-ink-900 border border-ink-800 hover:border-ink-600 hover:bg-ink-850 transition-colors text-left"
                  >
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="text-mist-200 truncate">{r.title}</span>
                        <StatusBadge status={r.status} />
                      </div>
                      <div className="text-xs text-ink-500 mt-1 flex gap-3 flex-wrap">
                        <span>{formatDateTime(r.recordedAt)}</span>
                        <span>{formatDuration(r.durationSec)}</span>
                      </div>
                    </div>
                  </button>
                ))}
              </div>
            )}
          </>
        ) : filtered.length === 0 ? (
          <div className="h-full flex flex-col items-center justify-center gap-3 text-center">
            <svg width="88" height="88" viewBox="0 0 1024 1024" className="opacity-60">
              <rect width="1024" height="1024" rx="230" fill="#14203a" />
              <g fill="#22d3ee" opacity="0.85">
                <rect x="120" y="392" width="52" height="96" rx="26" />
                <rect x="242" y="336" width="52" height="208" rx="26" />
                <rect x="364" y="284" width="52" height="312" rx="26" />
                <rect x="486" y="232" width="52" height="416" rx="26" />
                <rect x="608" y="284" width="52" height="312" rx="26" />
                <rect x="730" y="336" width="52" height="208" rx="26" />
                <rect x="852" y="392" width="52" height="96" rx="26" />
              </g>
              <line x1="170" y1="742" x2="786" y2="742" stroke="#34d399" strokeWidth="14" strokeLinecap="round" opacity="0.92" />
              <circle cx="856" cy="742" r="30" fill="#34d399" />
            </svg>
            <div className="text-mist-300">录音库还是空的</div>
            <div className="text-mist-400 text-sm">
              把手机录音（文件或整个文件夹）拖到这里，或点击右上角导入
            </div>
          </div>
        ) : (
          <div className="space-y-2">
            {filtered.map((r) => (
              <button
                key={r.id}
                onClick={() => go({ kind: "detail", id: r.id })}
                className="w-full flex items-center gap-4 px-4 py-3 rounded-xl bg-ink-900 border border-ink-800 hover:border-ink-600 hover:bg-ink-850 transition-colors text-left"
              >
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-mist-200 truncate">{r.title}</span>
                    <StatusBadge status={r.status} />
                  </div>
                  <div className="text-xs text-ink-500 mt-1 flex gap-3 flex-wrap">
                    <span>{formatDateTime(r.recordedAt)}</span>
                    <span>{formatDuration(r.durationSec)}</span>
                    <span>{formatBytes(r.sizeBytes)}</span>
                    <span className="uppercase">{r.format}</span>
                  </div>
                </div>
                <div className="flex gap-1.5 shrink-0">
                  {r.tags.slice(0, 4).map((t) => (
                    <span
                      key={t}
                      className="px-2 py-0.5 rounded-full bg-ink-800 text-ink-500 text-xs"
                    >
                      {t}
                    </span>
                  ))}
                </div>
              </button>
            ))}
          </div>
        )}
      </div>

      {/* 拖拽遮罩 */}
      {dragging && (
        <div className="absolute inset-0 z-10 flex items-center justify-center bg-ink-950/80 backdrop-blur-sm">
          <div className="border-2 border-dashed border-cyan-accent/60 rounded-2xl px-16 py-12 text-center">
            <div className="text-cyan-accent text-lg">松手导入录音</div>
            <div className="text-mist-400 text-sm mt-1">支持文件或文件夹（递归扫描）</div>
          </div>
        </div>
      )}
    </div>
  );
}
