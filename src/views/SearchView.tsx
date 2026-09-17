import { useEffect, useRef, useState } from "react";
import { errMsg, isTauri, searchAll, type SearchResults } from "../api";
import { useAppStore } from "../store";
import { formatDate, formatDuration, formatMsTime } from "../types";
import StatusBadge from "../components/StatusBadge";

/** 全文搜索：标题 / 标签 / 笔记 / 参会人 + 转写全文片段，命中可直达时间点 */
export default function SearchView() {
  const query = useAppStore((s) => s.libraryQuery);
  const setQuery = useAppStore((s) => s.setLibraryQuery);
  const recordings = useAppStore((s) => s.recordings);
  const go = useAppStore((s) => s.go);

  const [results, setResults] = useState<SearchResults | null>(null);
  const [error, setError] = useState("");
  const [searching, setSearching] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // 防抖 350ms
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setResults(null);
      setError("");
      return;
    }
    setSearching(true);
    const timer = setTimeout(() => {
      if (!isTauri()) {
        setSearching(false);
        return;
      }
      searchAll(q)
        .then((r) => {
          setResults(r);
          setError("");
        })
        .catch((e) => setError(errMsg(e)))
        .finally(() => setSearching(false));
    }, 350);
    return () => clearTimeout(timer);
  }, [query]);

  const q = query.trim();
  const hitRecordings = (results?.recordings ?? [])
    .map((hit) => recordings.find((r) => r.id === hit.id))
    .filter((r): r is NonNullable<typeof r> => !!r);

  const total = (results?.segments.length ?? 0) + hitRecordings.length;

  return (
    <div className="h-full flex flex-col">
      <div className="px-4 py-3 border-b border-ink-800 bg-ink-900/60">
        <div className="max-w-3xl mx-auto flex items-center gap-3">
          <div className="flex-1 relative">
            <input
              ref={inputRef}
              type="text"
              className="w-full pl-9"
              placeholder="搜索标题、标签、笔记、参会人，或转写稿里的任意一句话…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            <span className="absolute left-3 top-1/2 -translate-y-1/2 text-ink-500">
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round">
                <circle cx="11" cy="11" r="6.5" />
                <path d="m16 16 4 4" />
              </svg>
            </span>
          </div>
          {q && (
            <button
              className="text-xs text-mist-400 hover:text-mist-200 shrink-0"
              onClick={() => setQuery("")}
            >
              清空
            </button>
          )}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        <div className="max-w-3xl mx-auto p-4 space-y-4">
          {error && <div className="text-xs text-st-err">{error}</div>}

          {!q && (
            <div className="text-center py-16 space-y-2">
              <div className="text-mist-300 text-sm">输入关键词开始搜索</div>
              <div className="text-xs text-ink-500">
                中文按子串匹配，「客户 投诉」这类连续片段能直接定位到会议里的那一句话
              </div>
            </div>
          )}

          {q && searching && !results && (
            <div className="text-mist-400 text-sm py-8 text-center">搜索中…</div>
          )}

          {q && results && total === 0 && (
            <div className="text-center text-mist-400 text-sm py-16">
              没有找到与「{q}」相关的内容
            </div>
          )}

          {/* 转写片段命中 */}
          {q && (results?.segments.length ?? 0) > 0 && (
            <div className="space-y-2">
              <div className="text-xs text-ink-500 px-1">
                转写片段命中 {results!.segments.length} 条
              </div>
              {results!.segments.map((seg, i) => (
                <button
                  key={i}
                  onClick={() =>
                    go({ kind: "detail", id: seg.recordingId, seekMs: seg.startMs })
                  }
                  className="w-full text-left px-4 py-3 rounded-xl bg-ink-900 border border-ink-800 hover:border-ink-600 hover:bg-ink-850 transition-colors"
                >
                  <div className="flex items-center gap-2 text-xs text-ink-500">
                    <span className="text-cyan-accent tabular-nums shrink-0">
                      {formatMsTime(seg.startMs)}
                    </span>
                    <span className="truncate">{seg.recordingTitle}</span>
                    <span>·</span>
                    <span className="shrink-0">{formatDate(seg.recordedAt)}</span>
                  </div>
                  <div className="text-sm text-mist-300 mt-1 line-clamp-2">{seg.snippet}</div>
                </button>
              ))}
            </div>
          )}

          {/* 录音元数据命中 */}
          {q && hitRecordings.length > 0 && (
            <div className="space-y-2">
              <div className="text-xs text-ink-500 px-1">录音命中 {hitRecordings.length} 条</div>
              {hitRecordings.map((r) => (
                <button
                  key={r.id}
                  onClick={() => go({ kind: "detail", id: r.id })}
                  className="w-full flex items-center gap-4 px-4 py-3 rounded-xl bg-ink-900 border border-ink-800 hover:border-ink-600 hover:bg-ink-850 transition-colors text-left"
                >
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="text-sm text-mist-200 truncate">{r.title}</span>
                      <StatusBadge status={r.status} />
                    </div>
                    <div className="text-xs text-ink-500 mt-1 flex gap-3 flex-wrap">
                      <span className="tabular-nums">{formatDuration(r.durationSec)}</span>
                      {r.participants && <span className="truncate">{r.participants}</span>}
                      {r.tags.slice(0, 4).map((t) => (
                        <span key={t}>#{t}</span>
                      ))}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
