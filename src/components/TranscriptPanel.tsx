import { memo, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  cancelTranscribe,
  errMsg,
  listSegments,
  transcribeRecording,
  type JobEvent,
  type TranscriptSegment,
} from "../api";
import { formatMsTime, type Recording } from "../types";

/** 解析 chars JSON：[[char, ms], ...] */
function parseChars(chars: string): [string, number][] {
  try {
    const arr = JSON.parse(chars) as [string, number][];
    return Array.isArray(arr) ? arr : [];
  } catch {
    return [];
  }
}

/** 把文本按搜索词切分为普通/命中片段 */
function splitHighlight(text: string, query: string): { t: string; hit: boolean }[] {
  if (!query) return [{ t: text, hit: false }];
  const out: { t: string; hit: boolean }[] = [];
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  let i = 0;
  while (i < text.length) {
    const idx = lower.indexOf(q, i);
    if (idx === -1) {
      out.push({ t: text.slice(i), hit: false });
      break;
    }
    if (idx > i) out.push({ t: text.slice(i, idx), hit: false });
    out.push({ t: text.slice(idx, idx + q.length), hit: true });
    i = idx + q.length;
  }
  return out;
}

interface RowProps {
  seg: TranscriptSegment;
  index: number;
  isActive: boolean;
  query: string;
  onSeek: (sec: number) => void;
}

/**
 * 单个分段行。用 memo 隔离：播放时 currentTime 每秒变化多次，
 * 但只要「当前分段」没变，长列表（180 分钟会议可达数千段）就完全不重渲染。
 */
const SegmentRow = memo(function SegmentRow({
  seg,
  index,
  isActive,
  query,
  onSeek,
}: RowProps) {
  // 字符级时间轴仅在当前分段解析（其余分段用整段跳转，省内存与 CPU）
  const chars = useMemo(
    () => (isActive ? parseChars(seg.chars) : []),
    [isActive, seg.chars],
  );
  const q = query.trim().toLowerCase();

  return (
    <div
      data-seg={index}
      className={`group flex gap-3 px-2.5 py-1.5 rounded-lg cursor-pointer transition-colors ${
        isActive ? "bg-ink-800" : "hover:bg-ink-850"
      }`}
      onClick={() => onSeek(seg.startMs / 1000)}
      title={`跳到 ${formatMsTime(seg.startMs)}`}
    >
      <span
        className={`shrink-0 text-[11px] tabular-nums pt-0.5 select-none ${
          isActive ? "text-cyan-accent" : "text-ink-500"
        }`}
      >
        {formatMsTime(seg.startMs)}
      </span>
      <div className="text-sm leading-6 text-mist-200 flex-1">
        {chars.length > 0
          ? chars.map(([ch, ms], ci) => (
              <span
                key={ci}
                className={`hover:bg-cyan-accent/20 hover:text-cyan-accent rounded px-px ${
                  q && ch.toLowerCase().includes(q)
                    ? "bg-cyan-accent/30 text-mist-200"
                    : ""
                }`}
                onClick={(e) => {
                  e.stopPropagation();
                  onSeek(ms / 1000);
                }}
              >
                {ch}
              </span>
            ))
          : splitHighlight(seg.text, query.trim()).map(({ t, hit }, wi) =>
              hit ? (
                <mark key={wi} className="bg-cyan-accent/30 text-mist-200 rounded px-px">
                  {t}
                </mark>
              ) : (
                <span key={wi}>{t}</span>
              ),
            )}
      </div>
    </div>
  );
});

interface Props {
  rec: Recording;
  currentTime: number;
  seek: (sec: number) => void;
  /** 状态变化后通知父组件刷新录音元数据 */
  onChanged: () => void;
}

/** 转写稿面板：开始/取消转写、进度、点字跳转、跟随播放、稿内搜索 */
export default function TranscriptPanel({ rec, currentTime, seek, onChanged }: Props) {
  const [segments, setSegments] = useState<TranscriptSegment[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState(rec.status);
  const [progress, setProgress] = useState(0);
  const [error, setError] = useState("");
  const [follow, setFollow] = useState(true);
  const [search, setSearch] = useState("");
  const [matchIdx, setMatchIdx] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);

  // 初始载入（已完成转写的直接取分段）
  useEffect(() => {
    setStatus(rec.status);
    setError("");
    if (rec.status === "done") {
      listSegments(rec.id)
        .then((segs) => {
          setSegments(segs);
          setLoaded(true);
        })
        .catch((e) => setError(errMsg(e)));
    } else {
      setSegments([]);
      setLoaded(false);
    }
  }, [rec.id, rec.status]);

  // 任务进度事件
  useEffect(() => {
    const un = listen<JobEvent>("job://progress", (event) => {
      const p = event.payload;
      if (p.recordingId !== rec.id) return;
      setStatus(p.status === "done" ? "done" : p.status === "running" ? "transcribing" : p.status === "queued" ? "queued" : p.status === "failed" ? "failed" : "imported");
      setProgress(p.progress);
      if (p.error) setError(p.error);
      if (p.status === "done") {
        listSegments(rec.id)
          .then((segs) => {
            setSegments(segs);
            setLoaded(true);
          })
          .catch(() => undefined);
        onChanged();
      } else if (p.status === "failed" || p.status === "canceled") {
        onChanged();
      }
    });
    return () => {
      void un.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rec.id]);

  const matches = useMemo(() => {
    if (!search.trim()) return [];
    const q = search.trim().toLowerCase();
    return segments
      .map((s, i) => ({ i, seg: s }))
      .filter(({ seg }) => seg.text.toLowerCase().includes(q));
  }, [segments, search]);

  const curMs = currentTime * 1000;
  const activeIdx = useMemo(() => {
    let idx = -1;
    for (let i = 0; i < segments.length; i++) {
      if (segments[i].startMs <= curMs) idx = i;
      else break;
    }
    return idx;
  }, [segments, curMs]);

  // 跟随播放：滚动到当前分段
  useEffect(() => {
    if (!follow || activeIdx < 0 || !containerRef.current) return;
    const el = containerRef.current.querySelector(`[data-seg="${activeIdx}"]`);
    el?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [activeIdx, follow]);

  function startTranscribe() {
    setError("");
    transcribeRecording(rec.id)
      .then(() => setStatus("queued"))
      .catch((e) => setError(errMsg(e)));
  }

  function gotoMatch(delta: number) {
    if (matches.length === 0) return;
    const next = (matchIdx + delta + matches.length) % matches.length;
    setMatchIdx(next);
    const m = matches[next];
    seek(m.seg.startMs / 1000);
    containerRef.current
      ?.querySelector(`[data-seg="${m.i}"]`)
      ?.scrollIntoView({ block: "center", behavior: "smooth" });
  }

  return (
    <div className="rounded-xl border border-ink-800 bg-ink-900 p-4 flex flex-col min-h-[300px]">
      {/* 工具行 */}
      <div className="flex items-center gap-2 mb-3">
        <div className="text-mist-300 text-sm font-medium">转写稿</div>
        <div className="flex-1" />
        {status === "done" && (
          <>
            <input
              type="text"
              className="w-44 text-xs"
              placeholder="稿内搜索…"
              value={search}
              onChange={(e) => {
                setSearch(e.target.value);
                setMatchIdx(0);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") gotoMatch(e.shiftKey ? -1 : 1);
              }}
            />
            {search.trim() && (
              <span className="text-xs text-ink-500 tabular-nums">
                {matches.length ? matchIdx + 1 : 0}/{matches.length}
              </span>
            )}
            <button
              className="px-2 h-7 rounded bg-ink-700 hover:bg-ink-600 text-mist-300 text-xs"
              onClick={() => gotoMatch(-1)}
              title="上一个匹配（Shift+Enter）"
            >
              ‹
            </button>
            <button
              className="px-2 h-7 rounded bg-ink-700 hover:bg-ink-600 text-mist-300 text-xs"
              onClick={() => gotoMatch(1)}
              title="下一个匹配（Enter）"
            >
              ›
            </button>
            <label className="flex items-center gap-1 text-xs text-mist-400 cursor-pointer ml-1">
              <input
                type="checkbox"
                checked={follow}
                onChange={(e) => setFollow(e.target.checked)}
              />
              跟随
            </label>
          </>
        )}
      </div>

      {/* 状态区 */}
      {(status === "imported" || status === "failed") && (
        <div className="flex flex-col items-center gap-2 py-8">
          {status === "failed" && error && (
            <div className="text-xs text-red-300 max-w-md text-center">{error}</div>
          )}
          <button
            className="px-4 py-2 rounded-lg text-sm bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium hover:opacity-90"
            onClick={startTranscribe}
          >
            {status === "failed" ? "重新转写" : "开始转写"}
          </button>
          <div className="text-[11px] text-ink-500">
            本地离线转写 · 内容不出本机 · 首次需在设置中下载模型
          </div>
        </div>
      )}

      {(status === "queued" || status === "transcribing") && (
        <div className="flex flex-col items-center gap-3 py-8 w-full max-w-md mx-auto">
          <div className="text-sm text-mist-300">
            {status === "queued" ? "排队等待中…" : "正在转写（可继续其他操作）…"}
          </div>
          <div className="w-full h-2 rounded-full bg-ink-800 overflow-hidden">
            <div
              className="h-full bg-gradient-to-r from-teal-accent to-cyan-accent transition-all"
              style={{ width: `${Math.round(progress * 100)}%` }}
            />
          </div>
          <div className="text-xs text-ink-500 tabular-nums">
            {Math.round(progress * 100)}%
          </div>
          <button
            className="px-3 py-1.5 rounded-lg text-xs bg-ink-700 hover:bg-ink-600 text-mist-300"
            onClick={() => void cancelTranscribe(rec.id).catch((e) => setError(errMsg(e)))}
          >
            取消转写
          </button>
        </div>
      )}

      {status === "done" && (
        <div
          ref={containerRef}
          className="flex-1 overflow-y-auto max-h-[calc(100vh-430px)] min-h-[220px] pr-1 space-y-1"
        >
          {!loaded ? (
            <div className="text-mist-400 text-sm py-4">加载中…</div>
          ) : segments.length === 0 ? (
            <div className="text-mist-400 text-sm py-4">（无转写内容）</div>
          ) : (
            segments.map((seg, i) => (
              <SegmentRow
                key={seg.id}
                seg={seg}
                index={i}
                isActive={i === activeIdx}
                query={search}
                onSeek={seek}
              />
            ))
          )}
        </div>
      )}

      {status !== "done" && status !== "queued" && status !== "transcribing" && error && (
        <div className="text-xs text-red-300">{error}</div>
      )}
    </div>
  );
}
