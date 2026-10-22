import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  cancelTranscribe,
  errMsg,
  importPaths,
  transcribeRecording,
  type JobRow,
} from "../api";
import { useAppStore } from "../store";
import {
  formatBytes,
  formatDateTime,
  formatDuration,
  parseSummaryHeadline,
  type Recording,
} from "../types";
import StatusBadge from "../components/StatusBadge";
import DropMask from "../components/DropMask";
import { useAudioDrop } from "../hooks/useAudioDrop";

/** 该录音是否有排队中/进行中的任务 */
function isActive(r: Recording, jobs: JobRow[]): boolean {
  return jobs.some(
    (j) => j.recordingId === r.id && (j.status === "pending" || j.status === "running"),
  );
}

function RecordingRow({
  rec,
  job,
  selected,
  onToggle,
  onOpen,
  onTag,
}: {
  rec: Recording;
  job?: JobRow;
  selected: boolean;
  onToggle: () => void;
  onOpen: () => void;
  /** 点击行内标签 → 加入筛选（按事件找录音的主入口） */
  onTag: (tag: string) => void;
}) {
  const progress = job?.status === "running" ? job.progress : null;
  const headline = parseSummaryHeadline(rec.summaryMd);
  return (
    <div
      className={`group flex items-center gap-3 px-3 py-2.5 rounded-xl border transition-colors cursor-pointer ${
        selected
          ? "bg-ink-850 border-cyan-accent/50"
          : "bg-ink-900 border-ink-800 hover:border-ink-600 hover:bg-ink-850"
      }`}
      onClick={onOpen}
    >
      <label
        className="shrink-0 grid place-items-center w-5 h-5"
        onClick={(e) => e.stopPropagation()}
      >
        <input
          type="checkbox"
          className="w-4 h-4 accent-[var(--color-cyan-accent)] cursor-pointer"
          checked={selected}
          onChange={onToggle}
        />
      </label>

      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm text-mist-200 truncate">{rec.title}</span>
          {headline?.kind && (
            <span
              className="px-1.5 py-0.5 rounded bg-ink-800 text-[10px] text-cyan-accent shrink-0"
              title="由 AI 纪要识别的录音类型"
            >
              {headline.kind}
            </span>
          )}
          <StatusBadge status={rec.status} />
          {rec.summaryMd && <span className="text-[11px] text-st-ok shrink-0">有纪要</span>}
        </div>
        <div className="text-[11px] text-ink-500 mt-1 flex gap-3 flex-wrap items-center">
          <span className="tabular-nums">{formatDateTime(rec.recordedAt)}</span>
          <span className="tabular-nums">{formatDuration(rec.durationSec)}</span>
          <span>{formatBytes(rec.sizeBytes)}</span>
          <span className="uppercase">{rec.format}</span>
          {rec.participants && <span className="truncate max-w-[140px]">{rec.participants}</span>}
          {/* 标题已被主题替换时，原始文件名降级为溯源信息 */}
          {rec.origName !== rec.title && (
            <span className="truncate max-w-[180px]" title={`原始文件：${rec.origName}`}>
              {rec.origName}
            </span>
          )}
        </div>
        {progress !== null && (
          <div className="mt-1.5 flex items-center gap-2">
            <div className="flex-1 max-w-[200px] h-1 rounded-full bg-ink-800 overflow-hidden">
              <div
                className="h-full bg-gradient-to-r from-teal-accent to-cyan-accent transition-all"
                style={{ width: `${Math.round(progress * 100)}%` }}
              />
            </div>
            <span className="text-[10px] text-ink-500 tabular-nums">
              {Math.round(progress * 100)}%
            </span>
          </div>
        )}
      </div>

      <div className="flex gap-1.5 shrink-0 max-w-[220px]">
        {rec.tags.slice(0, 4).map((t) => (
          <button
            key={t}
            className="px-2 py-0.5 rounded-full bg-ink-800 text-ink-500 text-[11px] hover:text-cyan-accent transition-colors"
            title={`按标签「${t}」筛选`}
            onClick={(e) => {
              e.stopPropagation();
              onTag(t);
            }}
          >
            {t}
          </button>
        ))}
      </div>
    </div>
  );
}

export default function LibraryView() {
  const recordings = useAppStore((s) => s.recordings);
  const loaded = useAppStore((s) => s.recordingsLoaded);
  const jobs = useAppStore((s) => s.jobs);
  const go = useAppStore((s) => s.go);
  const refreshAll = useAppStore((s) => s.refreshAll);

  const [importing, setImporting] = useState(false);
  const [filter, setFilter] = useState("");
  const [statusFilter, setStatusFilter] = useState<"all" | "pending" | "done" | "failed">("all");
  // 标签筛选（可叠加，命中任一即显示）：按事件/主题找录音的主入口
  const [tagFilter, setTagFilter] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchMsg, setBatchMsg] = useState("");
  const lastResult = useAppStore((s) => s.lastImport);
  const setLastResult = useAppStore((s) => s.setLastImport);

  async function doImport(paths: string[] | null) {
    if (!paths || paths.length === 0) return;
    setImporting(true);
    try {
      const result = await importPaths(paths);
      setLastResult(result);
      await refreshAll();
    } catch (e) {
      setLastResult({ imported: [], skipped: [{ path: "", reason: errMsg(e) }] });
    } finally {
      setImporting(false);
    }
  }

  async function pickFiles() {
    let paths: string[] | string | null = null;
    try {
      paths = await open({
        multiple: true,
        filters: [
          {
            name: "音频",
            extensions: ["m4a", "mp3", "wav", "amr", "3gp", "wma", "aac", "flac", "ogg", "opus"],
          },
        ],
      });
    } catch {
      return;
    }
    await doImport(Array.isArray(paths) ? paths : paths ? [paths] : null);
  }

  async function pickFolder() {
    let dir: string | null = null;
    try {
      dir = await open({ directory: true, multiple: false });
    } catch {
      return;
    }
    await doImport(dir ? [dir] : null);
  }

  const dragging = useAudioDrop((paths) => void doImport(paths));

  // 全库标签及出现次数（LLM 标签 + 手工标签），按次数降序做筛选 chips
  const allTags = useMemo(() => {
    const counts = new Map<string, number>();
    for (const r of recordings) {
      for (const t of r.tags) counts.set(t, (counts.get(t) ?? 0) + 1);
    }
    return [...counts.entries()].sort(
      (a, b) => b[1] - a[1] || a[0].localeCompare(b[0], "zh"),
    );
  }, [recordings]);

  function toggleTag(tag: string) {
    setTagFilter((prev) => {
      const next = new Set(prev);
      if (next.has(tag)) next.delete(tag);
      else next.add(tag);
      return next;
    });
  }

  // 筛选后的列表
  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return recordings.filter((r) => {
      if (statusFilter === "pending" && r.status !== "imported" && r.status !== "queued" && r.status !== "transcribing")
        return false;
      if (statusFilter === "done" && r.status !== "done") return false;
      if (statusFilter === "failed" && r.status !== "failed") return false;
      if (tagFilter.size > 0 && !r.tags.some((t) => tagFilter.has(t))) return false;
      if (!q) return true;
      return (
        r.title.toLowerCase().includes(q) ||
        r.tags.some((t) => t.toLowerCase().includes(q)) ||
        r.participants.toLowerCase().includes(q) ||
        r.notes.toLowerCase().includes(q)
      );
    });
  }, [recordings, filter, statusFilter, tagFilter]);

  const jobOf = (id: number) =>
    jobs.find((j) => j.recordingId === id && (j.status === "pending" || j.status === "running"));

  // 选择集跟随筛选修剪：被筛掉/已删除的行自动移出选择，
  // 否则批量条的「已选 N 条」会把看不见的录音也算进去
  useEffect(() => {
    setSelected((prev) => {
      const ids = new Set(visible.map((r) => r.id));
      const next = new Set([...prev].filter((id) => ids.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [visible]);

  const selectedRecs = visible.filter((r) => selected.has(r.id));
  const selectedTodo = selectedRecs.filter((r) => !isActive(r, jobs));

  function toggle(id: number) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  /** 批量排队转写；已有任务在跑的已在 selectedTodo 里排除 */
  async function batchTranscribe() {
    if (selectedTodo.length === 0) return;
    setBatchBusy(true);
    setBatchMsg("");
    let queued = 0;
    const failed: string[] = [];
    for (const r of selectedTodo) {
      try {
        await transcribeRecording(r.id);
        queued += 1;
      } catch (e) {
        failed.push(errMsg(e));
      }
    }
    await refreshAll();
    setBatchBusy(false);
    setBatchMsg(
      `已排队 ${queued} 条${failed.length ? ` · ${failed.length} 条未加入（${failed[0] ?? ""}）` : ""}`,
    );
    setSelected(new Set());
  }

  async function batchCancel() {
    setBatchBusy(true);
    setBatchMsg("");
    let n = 0;
    for (const r of selectedRecs.filter((r) => isActive(r, jobs))) {
      try {
        await cancelTranscribe(r.id);
        n += 1;
      } catch {
        // 忽略单条失败
      }
    }
    await refreshAll();
    setBatchBusy(false);
    setBatchMsg(`已取消 ${n} 条排队任务`);
    setSelected(new Set());
  }

  return (
    <div className="h-full flex flex-col relative">
      {/* 工具栏 */}
      <div className="flex items-center gap-2 px-4 h-14 border-b border-ink-800 bg-ink-900/60 shrink-0">
        <input
          type="text"
          className="flex-1 max-w-xs"
          placeholder="按标题/标签/参会人筛选…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <div className="flex gap-1">
          {(
            [
              ["all", "全部"],
              ["pending", "待处理"],
              ["done", "已转写"],
              ["failed", "失败"],
            ] as const
          ).map(([key, label]) => (
            <button
              key={key}
              onClick={() => setStatusFilter(key)}
              className={`px-2.5 py-1 rounded-lg text-xs transition-colors ${
                statusFilter === key
                  ? "bg-ink-700 text-mist-100"
                  : "text-mist-400 hover:bg-ink-850 hover:text-mist-300"
              }`}
            >
              {label}
            </button>
          ))}
        </div>
        <div className="flex-1" />
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200 transition-colors disabled:opacity-50 shrink-0"
          onClick={pickFiles}
          disabled={importing}
        >
          导入文件
        </button>
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium hover:opacity-90 transition-opacity disabled:opacity-50 shrink-0"
          onClick={pickFolder}
          disabled={importing}
        >
          {importing ? "导入中…" : "导入文件夹"}
        </button>
      </div>

      {/* 标签筛选条：点击叠加筛选，再点取消 */}
      {allTags.length > 0 && (
        <div className="flex items-center gap-1.5 px-4 py-2 border-b border-ink-800 overflow-x-auto shrink-0">
          <span className="text-[11px] text-ink-500 shrink-0">标签</span>
          {allTags.map(([tag, n]) => (
            <button
              key={tag}
              onClick={() => toggleTag(tag)}
              className={`px-2.5 py-1 rounded-full text-xs whitespace-nowrap border transition-colors ${
                tagFilter.has(tag)
                  ? "bg-cyan-accent/15 border-cyan-accent/50 text-cyan-accent"
                  : "bg-ink-850 border-ink-800 text-ink-500 hover:text-mist-300 hover:border-ink-600"
              }`}
            >
              #{tag}
              <span className="ml-1 tabular-nums opacity-70">{n}</span>
            </button>
          ))}
        </div>
      )}

      {/* 提示条 */}
      {(lastResult || batchMsg) && (
        <div className="mx-4 mt-3 px-3 py-2 rounded-lg bg-ink-850 border border-ink-700 text-sm flex items-start gap-2 shrink-0">
          <div className="flex-1 text-mist-300">
            {lastResult && (
              <>
                导入完成：新增 {lastResult.imported.length} 条
                {lastResult.skipped.length > 0 && (
                  <span className="text-mist-400">
                    {" "}
                    · 跳过 {lastResult.skipped.length} 条（
                    {lastResult.skipped.map((s) => s.reason).join("；").slice(0, 100)}）
                  </span>
                )}
              </>
            )}
            {batchMsg && <span className={lastResult ? "ml-2 text-mist-400" : ""}>{batchMsg}</span>}
          </div>
          <button
            className="text-ink-500 hover:text-mist-300"
            onClick={() => {
              setLastResult(null);
              setBatchMsg("");
            }}
          >
            ✕
          </button>
        </div>
      )}

      {/* 全选行 */}
      {visible.length > 0 && (
        <div className="flex items-center gap-3 px-5 pt-3 pb-1 shrink-0">
          <label className="flex items-center gap-2 text-xs text-mist-400 cursor-pointer">
            <input
              type="checkbox"
              className="w-4 h-4 accent-[var(--color-cyan-accent)] cursor-pointer"
              checked={selected.size > 0 && selected.size === visible.length}
              onChange={(e) =>
                setSelected(e.target.checked ? new Set(visible.map((r) => r.id)) : new Set())
              }
            />
            全选
          </label>
          <span className="text-xs text-ink-500">
            共 {visible.length} 条
            {filter || statusFilter !== "all" || tagFilter.size > 0 ? "（已筛选）" : ""}
          </span>
          {(selected.size > 0 || tagFilter.size > 0) && (
            <>
              {selected.size > 0 && (
                <button
                  className="text-xs text-mist-400 hover:text-mist-200"
                  onClick={() => setSelected(new Set())}
                >
                  取消选择
                </button>
              )}
              {tagFilter.size > 0 && (
                <button
                  className="text-xs text-mist-400 hover:text-mist-200"
                  onClick={() => setTagFilter(new Set())}
                >
                  清除标签筛选
                </button>
              )}
            </>
          )}
        </div>
      )}

      {/* 列表 */}
      <div className="flex-1 overflow-y-auto p-4 pt-2 space-y-2">
        {!loaded ? (
          <div className="text-mist-400 text-sm">加载中…</div>
        ) : visible.length === 0 ? (
          <div className="h-full flex flex-col items-center justify-center gap-3 text-center">
            <svg width="88" height="88" viewBox="0 0 1024 1024" className="opacity-70">
              <rect width="1024" height="1024" rx="230" className="fill-ink-850" />
              <g className="fill-cyan-accent" opacity="0.85">
                <rect x="120" y="392" width="52" height="96" rx="26" />
                <rect x="242" y="336" width="52" height="208" rx="26" />
                <rect x="364" y="284" width="52" height="312" rx="26" />
                <rect x="486" y="232" width="52" height="416" rx="26" />
                <rect x="608" y="284" width="52" height="312" rx="26" />
                <rect x="730" y="336" width="52" height="208" rx="26" />
                <rect x="852" y="392" width="52" height="96" rx="26" />
              </g>
              <line
                x1="170"
                y1="742"
                x2="786"
                y2="742"
                className="stroke-jade-accent"
                strokeWidth="14"
                strokeLinecap="round"
                opacity="0.92"
              />
              <circle cx="856" cy="742" r="30" className="fill-jade-accent" />
            </svg>
            <div className="text-mist-300">
              {recordings.length === 0 ? "录音库还是空的" : "没有符合条件的录音"}
            </div>
            <div className="text-mist-400 text-sm">
              {recordings.length === 0
                ? "把手机录音（文件或整个文件夹）拖到这里，或点击右上角导入"
                : "换个关键词或状态筛选试试"}
            </div>
          </div>
        ) : (
          visible.map((r) => (
            <RecordingRow
              key={r.id}
              rec={r}
              job={jobOf(r.id)}
              selected={selected.has(r.id)}
              onToggle={() => toggle(r.id)}
              onOpen={() => go({ kind: "detail", id: r.id, from: "library" })}
              onTag={toggleTag}
            />
          ))
        )}
      </div>

      {/* 批量操作条 */}
      {selected.size > 0 && (
        <div className="absolute bottom-4 left-1/2 -translate-x-1/2 flex items-center gap-2 px-4 py-2.5 rounded-xl bg-ink-850 border border-ink-700 shadow-xl">
          <span className="text-sm text-mist-300 mr-1">已选 {selected.size} 条</span>
          <button
            className="px-3 py-1.5 rounded-lg text-sm bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium hover:opacity-90 disabled:opacity-50"
            onClick={batchTranscribe}
            disabled={batchBusy || selectedTodo.length === 0}
            title={
              selectedTodo.length === 0
                ? "所选录音都已有进行中的任务"
                : `排队转写 ${selectedTodo.length} 条`
            }
          >
            {batchBusy ? "处理中…" : `批量转写（${selectedTodo.length}）`}
          </button>
          <button
            className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200 disabled:opacity-50"
            onClick={batchCancel}
            disabled={batchBusy || selectedRecs.filter((r) => isActive(r, jobs)).length === 0}
          >
            取消排队
          </button>
          <button
            className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200"
            onClick={() => setSelected(new Set())}
          >
            取消选择
          </button>
        </div>
      )}

      {/* 拖拽遮罩 */}
      {dragging && <DropMask />}
    </div>
  );
}
