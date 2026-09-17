import { useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import {
  cancelTranscribe,
  errMsg,
  importPaths,
  isTauri,
  transcribeRecording,
  type JobRow,
} from "../api";
import { useAppStore } from "../store";
import {
  formatBytes,
  formatDateTime,
  formatDuration,
  type ImportResult,
  type Recording,
} from "../types";
import StatusBadge from "../components/StatusBadge";

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
}: {
  rec: Recording;
  job?: JobRow;
  selected: boolean;
  onToggle: () => void;
  onOpen: () => void;
}) {
  const progress = job?.status === "running" ? job.progress : null;
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
          <StatusBadge status={rec.status} />
          {rec.summaryMd && <span className="text-[11px] text-st-ok shrink-0">有纪要</span>}
        </div>
        <div className="text-[11px] text-ink-500 mt-1 flex gap-3 flex-wrap items-center">
          <span className="tabular-nums">{formatDateTime(rec.recordedAt)}</span>
          <span className="tabular-nums">{formatDuration(rec.durationSec)}</span>
          <span>{formatBytes(rec.sizeBytes)}</span>
          <span className="uppercase">{rec.format}</span>
          {rec.participants && <span className="truncate max-w-[140px]">{rec.participants}</span>}
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
          <span key={t} className="px-2 py-0.5 rounded-full bg-ink-800 text-ink-500 text-[11px]">
            {t}
          </span>
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

  const [dragging, setDragging] = useState(false);
  const [importing, setImporting] = useState(false);
  const [lastResult, setLastResult] = useState<ImportResult | null>(null);
  const [filter, setFilter] = useState("");
  const [statusFilter, setStatusFilter] = useState<"all" | "pending" | "done" | "failed">("all");
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchMsg, setBatchMsg] = useState("");
  const dragDepth = useRef(0);

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
    const paths = await open({
      multiple: true,
      filters: [
        {
          name: "音频",
          extensions: ["m4a", "mp3", "wav", "amr", "3gp", "wma", "aac", "flac", "ogg", "opus"],
        },
      ],
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

  // 筛选后的列表
  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return recordings.filter((r) => {
      if (statusFilter === "pending" && r.status !== "imported" && r.status !== "queued" && r.status !== "transcribing")
        return false;
      if (statusFilter === "done" && r.status !== "done") return false;
      if (statusFilter === "failed" && r.status !== "failed") return false;
      if (!q) return true;
      return (
        r.title.toLowerCase().includes(q) ||
        r.tags.some((t) => t.toLowerCase().includes(q)) ||
        r.participants.toLowerCase().includes(q) ||
        r.notes.toLowerCase().includes(q)
      );
    });
  }, [recordings, filter, statusFilter]);

  const jobOf = (id: number) =>
    jobs.find((j) => j.recordingId === id && (j.status === "pending" || j.status === "running"));

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
            共 {visible.length} 条{filter || statusFilter !== "all" ? "（已筛选）" : ""}
          </span>
          {selected.size > 0 && (
            <button
              className="text-xs text-mist-400 hover:text-mist-200"
              onClick={() => setSelected(new Set())}
            >
              取消选择
            </button>
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
              onOpen={() => go({ kind: "detail", id: r.id })}
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
      {dragging && (
        <div className="absolute inset-0 z-10 flex items-center justify-center bg-ink-950/60 backdrop-blur-sm">
          <div className="border-2 border-dashed border-cyan-accent/50 rounded-2xl px-16 py-12 text-center">
            <div className="text-cyan-accent text-lg">松手导入录音</div>
            <div className="text-mist-400 text-sm mt-1">支持文件或文件夹（递归扫描）</div>
          </div>
        </div>
      )}
    </div>
  );
}
