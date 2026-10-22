import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  cancelTranscribe,
  countEditedSegments,
  deleteSegment,
  errMsg,
  getSpeakerNames,
  listSegments,
  mergeSegments,
  mergeSpeakers,
  setSegmentSpeaker,
  setSpeakerNames,
  splitSegment as splitSegmentApi,
  transcribeRecording,
  updateSegment,
  type DiarEvent,
  type JobEvent,
  type TranscriptSegment,
} from "../api";
import { formatMsTime, type Recording } from "../types";
import { countText } from "../utils/wordCount";
import { splitHighlight } from "./Highlight";

/** 解析 chars JSON：[[char, ms], ...] */
function parseChars(chars: string): [string, number][] {
  try {
    const arr = JSON.parse(chars) as [string, number][];
    return Array.isArray(arr) ? arr : [];
  } catch {
    return [];
  }
}

const SPEAKER_TONE = ["text-spk-1", "text-spk-2", "text-spk-3"] as const;

function speakerTone(speaker: number | null): string {
  if (!speaker) return "text-ink-500";
  return SPEAKER_TONE[(speaker - 1) % SPEAKER_TONE.length];
}

interface RowProps {
  seg: TranscriptSegment;
  index: number;
  isActive: boolean;
  query: string;
  speakerName: string;
  canMergeDown: boolean;
  onSeek: (sec: number) => void;
  onSaveText: (id: number, text: string) => void;
  onDelete: (id: number) => void;
  onMergeDown: (index: number) => void;
  onSetSpeaker: (id: number, speaker: number | null) => void;
  onSplit: (index: number) => void;
  onDirtyChange: (dirty: boolean) => void;
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
  speakerName,
  canMergeDown,
  onSeek,
  onSaveText,
  onDelete,
  onMergeDown,
  onSetSpeaker,
  onSplit,
  onDirtyChange,
}: RowProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(seg.text);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // 字符级时间轴仅在当前分段解析（其余分段用整段跳转，省内存与 CPU）
  const chars = useMemo(
    () => (isActive ? parseChars(seg.chars) : []),
    [isActive, seg.chars],
  );
  const q = query.trim().toLowerCase();

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(draft.length, draft.length);
    }
  }, [editing, draft.length]);

  function beginEdit() {
    setDraft(seg.text);
    setEditing(true);
    onDirtyChange(true);
  }

  function commit() {
    setEditing(false);
    onDirtyChange(false);
    const next = draft.trim();
    if (next && next !== seg.text) onSaveText(seg.id, next);
  }

  function cancel() {
    setEditing(false);
    onDirtyChange(false);
    setDraft(seg.text);
  }

  return (
    <div
      data-seg={index}
      className={`group flex gap-2.5 px-2.5 py-1.5 rounded-lg transition-colors ${
        editing
          ? "bg-ink-850 ring-1 ring-cyan-accent/40"
          : isActive
            ? "bg-ink-800"
            : "hover:bg-ink-850"
      }`}
    >
      <button
        className={`shrink-0 text-[11px] tabular-nums pt-0.5 select-none w-12 text-left ${
          isActive ? "text-cyan-accent" : "text-ink-500 hover:text-cyan-accent"
        }`}
        onClick={() => onSeek(seg.startMs / 1000)}
        title={`跳到 ${formatMsTime(seg.startMs)}`}
      >
        {formatMsTime(seg.startMs)}
      </button>

      {/* 说话人 */}
      <div className="shrink-0 pt-0.5 w-20">
        {seg.speaker ? (
          <button
            className={`text-[11px] px-1 py-0.5 rounded hover:bg-ink-800/60 ${speakerTone(seg.speaker)}`}
            onClick={() => onSetSpeaker(seg.id, null)}
            title="点击清除说话人标注"
          >
            {speakerName}
          </button>
        ) : (
          <button
            className="text-[11px] px-1 py-0.5 rounded text-ink-500 opacity-0 group-hover:opacity-100 hover:bg-ink-800/60 hover:text-mist-300"
            onClick={() => onSetSpeaker(seg.id, 1)}
            title="标注说话人"
          >
            + 说话人
          </button>
        )}
      </div>

      <div className="flex-1 min-w-0">
        {editing ? (
          <div className="space-y-1.5">
            <textarea
              ref={inputRef}
              className="w-full min-h-[64px] resize-y text-sm leading-6"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  cancel();
                } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                  e.preventDefault();
                  commit();
                }
              }}
            />
            <div className="flex items-center gap-2">
              <button
                className="px-2.5 py-1 rounded text-xs bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium"
                onClick={commit}
              >
                保存
              </button>
              <button
                className="px-2.5 py-1 rounded text-xs bg-ink-700 hover:bg-ink-600 text-mist-300"
                onClick={cancel}
              >
                取消
              </button>
              {chars.length > 0 && (
                <span className="text-[11px] text-ink-500">
                  在下方原文中点一个字符可从此处拆分
                </span>
              )}
              <div className="flex-1" />
              <span className="text-[11px] text-ink-500">Ctrl+Enter 保存 · Esc 取消</span>
            </div>
          </div>
        ) : (
          <div
            className="text-sm leading-6 text-mist-200 cursor-text"
            onDoubleClick={beginEdit}
            title="双击修改文字"
          >
            {chars.length > 0
              ? chars.map(([ch, ms], ci) => (
                  <span
                    key={ci}
                    className={`hover:bg-cyan-accent/20 hover:text-cyan-accent rounded px-px ${
                      q && ch.toLowerCase().includes(q) ? "bg-cyan-accent/30 text-mist-200" : ""
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
        )}
        {seg.edited && !editing && (
          <span className="text-[10px] text-ink-500 mt-0.5 inline-block">已手工修改</span>
        )}
      </div>

      {!editing && (
        <div className="shrink-0 flex items-start gap-0.5 pt-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
          <button
            className="px-1.5 py-0.5 rounded text-[11px] text-mist-400 hover:bg-ink-700 hover:text-mist-100"
            onClick={beginEdit}
            title="修改这段文字"
          >
            ✎
          </button>
          <button
            className="px-1.5 py-0.5 rounded text-[11px] text-mist-400 hover:bg-ink-700 hover:text-mist-100"
            onClick={() => onSplit(index)}
            title="从时间中点拆成两段"
          >
            拆
          </button>
          {canMergeDown && (
            <button
              className="px-1.5 py-0.5 rounded text-[11px] text-mist-400 hover:bg-ink-700 hover:text-mist-100"
              onClick={() => onMergeDown(index)}
              title="与下一段合并"
            >
              ⧉
            </button>
          )}
          <button
            className="px-1.5 py-0.5 rounded text-[11px] text-mist-400 hover:bg-st-err-soft hover:text-st-err"
            onClick={() => onDelete(seg.id)}
            title="删除这一段"
          >
            ✕
          </button>
        </div>
      )}
    </div>
  );
});

interface Props {
  rec: Recording;
  currentTime: number;
  seek: (sec: number) => void;
  /** 状态变化后通知父组件刷新录音元数据 */
  onChanged: () => void;
  /** 有未保存的编辑草稿时通知父组件（用于离开前的确认拦截） */
  onDirtyChange?: (dirty: boolean) => void;
}

/** 转写稿面板：开始/取消转写、进度、点字跳转、跟随播放、稿内搜索、编辑与说话人标注 */
export default function TranscriptPanel({ rec, currentTime, seek, onChanged, onDirtyChange }: Props) {
  const [segments, setSegments] = useState<TranscriptSegment[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState(rec.status);
  const [progress, setProgress] = useState(0);
  const [error, setError] = useState("");
  const [follow, setFollow] = useState(true);
  const [search, setSearch] = useState("");
  const [matchIdx, setMatchIdx] = useState(0);
  const [names, setNames] = useState<Record<string, string>>({});
  const [speakerFilter, setSpeakerFilter] = useState<number | "all">("all");
  const [editedCount, setEditedCount] = useState(0);
  const [dirty, setDirty] = useState(false);
  const [editingName, setEditingName] = useState<number | null>(null);
  const [nameDraft, setNameDraft] = useState("");
  // 合并说话人（修正自动分离把同一人拆成多簇）：from 簇整体并入 to 簇
  const [mergeOpen, setMergeOpen] = useState(false);
  const [mergeFrom, setMergeFrom] = useState<number | null>(null);
  const [mergeTo, setMergeTo] = useState<number | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  // 把脏标记转发给父组件（详情页据此拦截切页签/返回）
  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  const reload = useCallback(() => {
    return listSegments(rec.id)
      .then((segs) => {
        setSegments(segs);
        setLoaded(true);
        // 分段已重建（合并/拆分/重新转写），行内未保存的草稿已随旧行消失
        setDirty(false);
      })
      .catch((e) => {
        setError(errMsg(e));
        setLoaded(true);
      });
  }, [rec.id]);

  const refreshEdited = useCallback(() => {
    countEditedSegments(rec.id)
      .then(setEditedCount)
      .catch(() => undefined);
  }, [rec.id]);

  // 初始载入
  useEffect(() => {
    setStatus(rec.status);
    setError("");
    setSpeakerFilter("all");
    getSpeakerNames(rec.id)
      .then(setNames)
      .catch(() => undefined);
    if (rec.status === "done") {
      void reload();
      refreshEdited();
    } else {
      setSegments([]);
      setLoaded(false);
    }
  }, [rec.id, rec.status, reload, refreshEdited]);

  // 任务进度事件
  useEffect(() => {
    const un = listen<JobEvent>("job://progress", (event) => {
      const p = event.payload;
      if (p.recordingId !== rec.id) return;
      setStatus(
        p.status === "done"
          ? "done"
          : p.status === "running"
            ? "transcribing"
            : p.status === "queued"
              ? "queued"
              : p.status === "failed"
                ? "failed"
                : "imported",
      );
      setProgress(p.progress);
      if (p.error) setError(p.error);
      if (p.status === "done") {
        void reload();
        refreshEdited();
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

  // 自动分离完成：分段与说话人名都可能变了，刷新两者
  useEffect(() => {
    const un = listen<DiarEvent>("diar://progress", (event) => {
      const p = event.payload;
      if (p.recordingId !== rec.id || p.status !== "done") return;
      void reload();
      getSpeakerNames(rec.id).then(setNames).catch(() => undefined);
    });
    return () => {
      void un.then((fn) => fn());
    };
  }, [rec.id, reload]);

  const speakerName = useCallback(
    (n: number) => names[String(n)] || `说话人${n}`,
    [names],
  );

  /** 稿子里出现过的说话人编号 */
  const speakerIds = useMemo(
    () => [...new Set(segments.map((s) => s.speaker).filter((s): s is number => !!s))].sort((a, b) => a - b),
    [segments],
  );

  const visible = useMemo(
    () => (speakerFilter === "all" ? segments : segments.filter((s) => s.speaker === speakerFilter)),
    [segments, speakerFilter],
  );

  // 全稿字数：从 text 派生（chars 是时间轴 JSON，编辑后会被清空，不可靠）；
  // 只随分段变化重算，与逐帧播放渲染隔离
  const wordCount = useMemo(() => {
    let n = 0;
    for (const s of segments) n += countText(s.text).words;
    return n;
  }, [segments]);

  const matches = useMemo(() => {
    if (!search.trim()) return [];
    const q = search.trim().toLowerCase();
    return visible
      .map((s, i) => ({ i, seg: s }))
      .filter(({ seg }) => seg.text.toLowerCase().includes(q));
  }, [visible, search]);

  const curMs = currentTime * 1000;
  const activeIdx = useMemo(() => {
    let idx = -1;
    for (let i = 0; i < visible.length; i++) {
      if (visible[i].startMs <= curMs) idx = i;
      else break;
    }
    return idx;
  }, [visible, curMs]);

  // 跟随播放：滚动到当前分段
  useEffect(() => {
    if (!follow || activeIdx < 0 || !containerRef.current) return;
    const el = containerRef.current.querySelector(`[data-seg="${activeIdx}"]`);
    el?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [activeIdx, follow]);

  function startTranscribe() {
    setError("");
    const doIt = () => {
      transcribeRecording(rec.id)
        .then(() => setStatus("queued"))
        .catch((e) => setError(errMsg(e)));
    };
    if (editedCount > 0) {
      const ok = window.confirm(
        `这条录音有 ${editedCount} 处手工修改（改字 / 合并 / 说话人标注）。\n\n` +
          `重新转写会用新结果覆盖全部分段，这些修改将无法恢复。\n是否继续？`,
      );
      if (!ok) return;
    }
    doIt();
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

  // ---- 编辑操作 ----

  const saveText = useCallback(
    (id: number, text: string) => {
      updateSegment(id, text)
        .then(() => {
          setSegments((prev) => prev.map((s) => (s.id === id ? { ...s, text, chars: "[]", edited: true } : s)));
          refreshEdited();
        })
        .catch((e) => setError(errMsg(e)));
    },
    [refreshEdited],
  );

  const removeSegment = useCallback(
    (id: number) => {
      if (!window.confirm("删除这一段？转写稿里的这句话会被移除（音频不受影响）。")) return;
      deleteSegment(id)
        .then(() => {
          setSegments((prev) => prev.filter((s) => s.id !== id));
          refreshEdited();
        })
        .catch((e) => setError(errMsg(e)));
    },
    [refreshEdited],
  );

  const mergeDown = useCallback(
    (index: number) => {
      const cur = visible[index];
      const next = visible[index + 1];
      if (!cur || !next) return;
      const charsLost = cur.chars !== "[]" && next.chars !== "[]";
      if (charsLost) {
        const ok = window.confirm(
          "两段都带字符级时间轴。合并后该段将改用整段跳转（点字跳转不可用），是否继续？",
        );
        if (!ok) return;
      }
      mergeSegments([cur.id, next.id])
        .then(() => reload())
        .then(refreshEdited)
        .catch((e) => setError(errMsg(e)));
    },
    [visible, reload, refreshEdited],
  );

  const setSpeaker = useCallback(
    (id: number, speaker: number | null) => {
      setSegmentSpeaker(id, speaker)
        .then(() => {
          setSegments((prev) => prev.map((s) => (s.id === id ? { ...s, speaker, edited: true } : s)));
          refreshEdited();
        })
        .catch((e) => setError(errMsg(e)));
    },
    [refreshEdited],
  );

  /** 合并说话人：把 from 簇并入 to 簇（修正自动分离把同一人拆成多簇） */
  const applyMerge = useCallback(() => {
    if (!mergeFrom || !mergeTo || mergeFrom === mergeTo) return;
    const count = segments.filter((s) => s.speaker === mergeFrom).length;
    const ok = window.confirm(
      `把「${speakerName(mergeFrom)}」的 ${count} 段并入「${speakerName(mergeTo)}」？\n\n` +
        `合并是手工修正：这些段之后的自动分离不会再覆盖它们。`,
    );
    if (!ok) return;
    mergeSpeakers(rec.id, mergeFrom, mergeTo)
      .then(() => reload())
      .then(() => getSpeakerNames(rec.id).then(setNames))
      .then(refreshEdited)
      .then(() => {
        setMergeOpen(false);
        setMergeFrom(null);
        setMergeTo(null);
      })
      .catch((e) => setError(errMsg(e)));
  }, [mergeFrom, mergeTo, segments, speakerName, rec.id, reload, refreshEdited]);

  /** 从时间中点把一段拆成两段（后端原子操作，两半各自保留时间轴） */
  const splitSegment = useCallback(
    (index: number) => {
      const seg = visible[index];
      if (!seg) return;
      if (seg.endMs - seg.startMs < 600) {
        setError("这一段不足 0.6 秒，拆开没有意义");
        return;
      }
      setError("");
      const midMs = Math.round((seg.startMs + seg.endMs) / 2);
      splitSegmentApi(seg.id, midMs)
        .then(() => reload())
        .then(refreshEdited)
        .catch((e) => setError(errMsg(e)));
    },
    [visible, reload, refreshEdited],
  );

  return (
    <div className="rounded-xl border border-ink-800 bg-ink-900 flex flex-col min-h-[320px] flex-1 min-w-0">
      {/* 工具行 */}
      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-ink-800 flex-wrap">
        <div className="text-mist-300 text-sm font-medium">转写稿</div>
        {status === "done" && segments.length > 0 && (
          <span
            className="text-[11px] text-ink-500"
            title="中文逐字计数，英文/数字按词计数，不含标点"
          >
            {segments.length} 段 · {wordCount.toLocaleString()} 字
            {editedCount > 0 && <span className="ml-1.5 text-st-warn">· 手工修改 {editedCount} 处</span>}
          </span>
        )}
        <div className="flex-1" />

        {status === "done" && (
          <>
            {speakerIds.length > 0 && (
              <>
                <select
                  className="text-xs py-1"
                  value={String(speakerFilter)}
                  onChange={(e) =>
                    setSpeakerFilter(e.target.value === "all" ? "all" : Number(e.target.value))
                  }
                  title="按说话人筛选"
                >
                  <option value="all">全部说话人</option>
                  {speakerIds.map((n) => (
                    <option key={n} value={n}>
                      {speakerName(n)}
                    </option>
                  ))}
                </select>
                {editingName !== null ? (
                  <input
                    type="text"
                    className="w-24 text-xs py-1"
                    autoFocus
                    value={nameDraft}
                    placeholder={`说话人${editingName}`}
                    onChange={(e) => setNameDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        const next = { ...names, [String(editingName)]: nameDraft.trim() };
                        if (!nameDraft.trim()) delete next[String(editingName)];
                        setNames(next);
                        setSpeakerNames(rec.id, next).catch(() => undefined);
                        setEditingName(null);
                      } else if (e.key === "Escape") {
                        setEditingName(null);
                      }
                    }}
                    onBlur={() => setEditingName(null)}
                  />
                ) : (
                  <button
                    className="text-xs text-mist-400 hover:text-cyan-accent px-1"
                    onClick={() => {
                      setEditingName(speakerIds[0]);
                      setNameDraft(names[String(speakerIds[0])] ?? "");
                    }}
                    title="给说话人起名字（如「张三」）"
                  >
                    改名
                  </button>
                )}
                {speakerIds.length >= 2 && (
                  <button
                    className={`text-xs px-1.5 py-0.5 rounded ${
                      mergeOpen
                        ? "bg-cyan-accent/20 text-cyan-accent"
                        : "text-mist-400 hover:text-cyan-accent"
                    }`}
                    onClick={() => setMergeOpen((v) => !v)}
                    title="自动分离把同一人拆成了多个？在这里把它们并成一个"
                  >
                    合并
                  </button>
                )}
              </>
            )}

            <input
              type="text"
              className="w-40 text-xs py-1"
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
            <label className="flex items-center gap-1 text-xs text-mist-400 cursor-pointer">
              <input
                type="checkbox"
                className="accent-[var(--color-cyan-accent)]"
                checked={follow}
                onChange={(e) => setFollow(e.target.checked)}
              />
              跟随
            </label>
          </>
        )}
      </div>

      {/* 合并说话人（自动分离把同一人拆成多簇时的一击修正） */}
      {mergeOpen && speakerIds.length >= 2 && (
        <div className="flex items-center gap-2 px-4 py-2 border-b border-ink-800 text-xs flex-wrap">
          <span className="text-ink-500">把</span>
          <select
            className="text-xs py-1"
            value={mergeFrom ?? ""}
            onChange={(e) => setMergeFrom(e.target.value ? Number(e.target.value) : null)}
          >
            <option value="">选择说话人…</option>
            {speakerIds.map((n) => (
              <option key={n} value={n}>
                {speakerName(n)}
              </option>
            ))}
          </select>
          <span className="text-ink-500">并入</span>
          <select
            className="text-xs py-1"
            value={mergeTo ?? ""}
            onChange={(e) => setMergeTo(e.target.value ? Number(e.target.value) : null)}
          >
            <option value="">选择说话人…</option>
            {speakerIds
              .filter((n) => n !== mergeFrom)
              .map((n) => (
                <option key={n} value={n}>
                  {speakerName(n)}
                </option>
              ))}
          </select>
          <button
            className="px-2.5 py-1 rounded bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium disabled:opacity-50"
            disabled={!mergeFrom || !mergeTo || mergeFrom === mergeTo}
            onClick={applyMerge}
          >
            合并
          </button>
          <span className="text-[11px] text-ink-500">被并入的段会标记为手工修正，之后自动分离不再覆盖</span>
        </div>
      )}

      {/* 状态区 */}
      {(status === "imported" || status === "failed") && (
        <div className="flex flex-col items-center gap-2 py-8">
          {status === "failed" && error && (
            <div className="text-xs text-st-err max-w-md text-center">{error}</div>
          )}
          <button
            className="px-4 py-2 rounded-lg text-sm bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium hover:opacity-90"
            onClick={startTranscribe}
          >
            {status === "failed" ? "重新转写" : "开始转写"}
          </button>
          {editedCount > 0 && (
            <div className="text-[11px] text-st-warn">
              注意：这条录音有 {editedCount} 处手工修改，重新转写会覆盖它们
            </div>
          )}
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
          <div className="text-xs text-ink-500 tabular-nums">{Math.round(progress * 100)}%</div>
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
          className="flex-1 overflow-y-auto p-2 space-y-0.5 min-h-[220px]"
          onInput={() => undefined}
        >
          {!loaded ? (
            <div className="text-mist-400 text-sm py-4 px-2">加载中…</div>
          ) : visible.length === 0 ? (
            <div className="text-mist-400 text-sm py-4 px-2">
              {segments.length === 0 ? "（无转写内容）" : "该说话人没有分段"}
            </div>
          ) : (
            visible.map((seg, i) => (
              <SegmentRow
                key={seg.id}
                seg={seg}
                index={i}
                isActive={i === activeIdx}
                query={search}
                speakerName={seg.speaker ? speakerName(seg.speaker) : ""}
                canMergeDown={speakerFilter === "all" && i < visible.length - 1}
                onSeek={seek}
                onSaveText={saveText}
                onDelete={removeSegment}
                onMergeDown={mergeDown}
                onSetSpeaker={setSpeaker}
                onSplit={splitSegment}
                onDirtyChange={setDirty}
              />
            ))
          )}
        </div>
      )}

      {status === "done" && (
        <div className="px-4 py-2 border-t border-ink-800 text-[11px] text-ink-500 flex items-center gap-3">
          <span>双击段落改文字 · ⧉ 合并下一段 · ✕ 删段 · 点时间戳跳音频</span>
          {dirty && <span className="text-st-warn">有未保存的编辑</span>}
        </div>
      )}

      {status !== "done" && status !== "queued" && status !== "transcribing" && error && (
        <div className="px-4 py-2 text-xs text-st-err">{error}</div>
      )}
    </div>
  );
}
