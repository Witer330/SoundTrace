import { useCallback, useEffect, useState } from "react";
import {
  countEditedSegments,
  ensurePeaks,
  errMsg,
  generateSummary,
  listBookmarks,
  listSegments,
  modelsStatus,
  transcribeRecording,
} from "../api";
import type { JobRow } from "../api";
import { formatBytes, formatDateTime, formatDuration, formatMsTime, type Recording } from "../types";
import StatusBadge from "./StatusBadge";

type StageState = "done" | "pending" | "running" | "failed" | "idle";

function StageIcon({ state }: { state: StageState }) {
  if (state === "done") {
    return (
      <span className="w-4 h-4 rounded-full grid place-items-center bg-st-ok-soft text-st-ok text-[9px] shrink-0">
        ✓
      </span>
    );
  }
  if (state === "running") {
    return <span className="w-4 h-4 rounded-full bg-st-info animate-pulse shrink-0" />;
  }
  if (state === "failed") {
    return (
      <span className="w-4 h-4 rounded-full grid place-items-center bg-st-err-soft text-st-err text-[9px] shrink-0">
        !
      </span>
    );
  }
  return <span className="w-4 h-4 rounded-full border border-ink-600 shrink-0" />;
}

function Stage({
  name,
  state,
  lines,
  action,
}: {
  name: string;
  state: StageState;
  lines: { label: string; value: string; tone?: string }[];
  action?: { label: string; onClick: () => void; busy?: boolean; disabled?: boolean; danger?: boolean };
}) {
  return (
    <div className="relative pl-5 pb-3.5 last:pb-0">
      {/* 竖向连接线 */}
      <span className="absolute left-[7px] top-5 bottom-0 w-px bg-ink-800 last:hidden" />
      <span className="absolute left-0 top-0.5">
        <StageIcon state={state} />
      </span>
      <div className="flex items-baseline gap-2">
        <span
          className={`text-[13px] ${
            state === "done" ? "text-mist-200" : state === "running" ? "text-mist-100" : "text-mist-400"
          }`}
        >
          {name}
        </span>
        {state === "running" && <span className="text-[10px] text-st-info">进行中</span>}
      </div>
      <div className="mt-0.5 space-y-0.5">
        {lines.map((l, i) => (
          <div key={i} className="flex items-baseline gap-1.5 text-[11px]">
            <span className="text-ink-500 shrink-0">{l.label}</span>
            <span className={`tabular-nums truncate ${l.tone ?? "text-mist-300"}`} title={l.value}>
              {l.value}
            </span>
          </div>
        ))}
      </div>
      {action && (
        <button
          className={`mt-1.5 px-2 py-1 rounded text-[11px] transition-colors disabled:opacity-50 ${
            action.danger
              ? "bg-st-err-soft hover:bg-st-err/20 text-st-err"
              : "bg-ink-800 hover:bg-ink-700 text-mist-300"
          }`}
          onClick={action.onClick}
          disabled={action.busy || action.disabled}
        >
          {action.busy ? "处理中…" : action.label}
        </button>
      )}
    </div>
  );
}

interface Props {
  rec: Recording;
  job?: JobRow;
  currentTime: number;
  segmentCount: number;
  peaksReady: boolean;
  onSeek: (sec: number) => void;
  onChanged: () => void;
}

/**
 * 处理流水线：把一条录音从进库到出纪要的过程拆成可见的五步，
 * 每步显示产物与状态、可单独重跑。这是「录音处理比通用工作台更细」的落点。
 */
export default function PipelinePanel({
  rec,
  job,
  currentTime,
  segmentCount,
  peaksReady,
  onSeek,
  onChanged,
}: Props) {
  const [editedCount, setEditedCount] = useState(0);
  const [speakerCount, setSpeakerCount] = useState(0);
  const [speakerTotal, setSpeakerTotal] = useState(0);
  const [bookmarkCount, setBookmarkCount] = useState(0);
  const [modelsReady, setModelsReady] = useState(true);
  const [busy, setBusy] = useState<"" | "peaks" | "transcribe" | "summary">("");
  const [error, setError] = useState("");

  const load = useCallback(() => {
    listSegments(rec.id)
      .then((segs) => {
        setEditedCount(segs.filter((s) => s.edited).length);
        setSpeakerTotal(segs.filter((s) => s.speaker).length);
        setSpeakerCount(new Set(segs.filter((s) => s.speaker).map((s) => s.speaker)).size);
      })
      .catch(() => undefined);
    countEditedSegments(rec.id)
      .then(setEditedCount)
      .catch(() => undefined);
    listBookmarks(rec.id)
      .then((b) => setBookmarkCount(b.length))
      .catch(() => undefined);
  }, [rec.id]);

  useEffect(() => {
    load();
    modelsStatus()
      .then((ms) => setModelsReady(ms.every((m) => m.ready)))
      .catch(() => undefined);
  }, [load, rec.status]);

  const runPeaks = () => {
    setBusy("peaks");
    setError("");
    ensurePeaks(rec.id)
      .then(() => onChanged())
      .catch((e) => setError(errMsg(e)))
      .finally(() => setBusy(""));
  };

  const runTranscribe = () => {
    if (editedCount > 0) {
      const ok = window.confirm(
        `这条录音有 ${editedCount} 处手工修改（改字 / 合并 / 拆分 / 说话人标注）。\n\n` +
          `重新转写会用新结果覆盖全部分段，这些修改将无法恢复。\n是否继续？`,
      );
      if (!ok) return;
    }
    setBusy("transcribe");
    setError("");
    transcribeRecording(rec.id)
      .then(() => {
        load();
        onChanged();
      })
      .catch((e) => setError(errMsg(e)))
      .finally(() => setBusy(""));
  };

  const runSummary = () => {
    setBusy("summary");
    setError("");
    generateSummary(rec.id)
      .catch((e) => setError(errMsg(e)))
      .finally(() => setBusy(""));
  };

  // ---- 各阶段状态 ----
  const transcribeRunning = job?.status === "running";
  const transcribeQueued = job?.status === "pending";
  const transcribeState: StageState = transcribeRunning
    ? "running"
    : transcribeQueued
      ? "pending"
      : rec.status === "done"
        ? "done"
        : rec.status === "failed"
          ? "failed"
          : "idle";

  const speakerState: StageState = speakerCount > 0 ? "done" : "idle";
  const summaryState: StageState = rec.summaryMd ? "done" : rec.status === "done" ? "idle" : "pending";

  return (
    <aside className="w-60 shrink-0 border-r border-ink-800 bg-ink-900/50 overflow-y-auto">
      <div className="p-4 space-y-3">
        <div className="flex items-center gap-2">
          <span className="text-mist-300 text-sm font-medium">处理流水线</span>
          <div className="flex-1" />
          <StatusBadge status={rec.status} />
        </div>

        <div>
          <Stage
            name="归档入库"
            state="done"
            lines={[
              { label: "时间", value: formatDateTime(rec.createdAt) },
              { label: "大小", value: formatBytes(rec.sizeBytes) },
              { label: "格式", value: rec.format.toUpperCase() },
            ]}
          />

          <Stage
            name="波形峰值"
            state={peaksReady ? "done" : "idle"}
            lines={
              peaksReady
                ? [{ label: "分辨率", value: "100ms / 点" }]
                : [{ label: "状态", value: "未生成", tone: "text-ink-500" }]
            }
            action={{ label: peaksReady ? "重新生成" : "生成波形", onClick: runPeaks, busy: busy === "peaks" }}
          />

          <Stage
            name="离线转写"
            state={transcribeState}
            lines={
              transcribeRunning
                ? [
                    { label: "进度", value: `${Math.round((job?.progress ?? 0) * 100)}%`, tone: "text-st-info" },
                    {
                      label: "剩余",
                      value: formatDuration(
                        Math.max(
                          0,
                          (rec.durationSec * (1 - (job?.progress ?? 0))) / 8,
                        ),
                      ),
                      tone: "text-ink-500",
                    },
                  ]
                : transcribeQueued
                  ? [{ label: "状态", value: "排队等待中", tone: "text-st-warn" }]
                  : rec.status === "done"
                    ? [
                        { label: "分段", value: `${segmentCount} 段` },
                        { label: "结束", value: formatDateTime(job?.updatedAt ?? rec.updatedAt) },
                      ]
                    : rec.status === "failed"
                      ? [{ label: "原因", value: job?.error ?? "转写失败", tone: "text-st-err" }]
                      : [{ label: "状态", value: "未转写", tone: "text-ink-500" }]
            }
            action={
              rec.status === "done" || rec.status === "failed" || rec.status === "imported"
                ? {
                    label: rec.status === "done" ? "重新转写" : "开始转写",
                    onClick: runTranscribe,
                    busy: busy === "transcribe",
                    disabled: !modelsReady || transcribeRunning || transcribeQueued,
                    danger: rec.status === "done" && editedCount > 0,
                  }
                : undefined
            }
          />

          <Stage
            name="说话人标注"
            state={speakerState}
            lines={
              speakerTotal === 0
                ? [
                    { label: "状态", value: "未标注", tone: "text-ink-500" },
                    { label: "提示", value: "在转写稿里点「+ 说话人」", tone: "text-ink-500" },
                  ]
                : [
                    { label: "说话人", value: `${speakerCount} 人` },
                    { label: "已标注", value: `${speakerTotal} 段` },
                  ]
            }
          />

          <Stage
            name="AI 纪要"
            state={summaryState}
            lines={
              rec.summaryMd
                ? [
                    { label: "模型", value: rec.llmModel ?? "—" },
                    { label: "字数", value: `${rec.summaryMd.length} 字` },
                  ]
                : rec.status === "done"
                  ? [{ label: "状态", value: "未生成", tone: "text-ink-500" }]
                  : [{ label: "前置", value: "需先完成转写", tone: "text-ink-500" }]
            }
            action={
              rec.status === "done"
                ? {
                    label: rec.summaryMd ? "重新生成" : "生成纪要",
                    onClick: runSummary,
                    busy: busy === "summary",
                  }
                : undefined
            }
          />
        </div>

        {/* 概览与快捷操作 */}
        <div className="pt-3 border-t border-ink-800 space-y-1.5 text-[11px]">
          <div className="flex justify-between">
            <span className="text-ink-500">手工修改</span>
            <span className={editedCount > 0 ? "text-st-warn tabular-nums" : "text-mist-300"}>
              {editedCount > 0 ? `${editedCount} 处` : "—"}
            </span>
          </div>
          <div className="flex justify-between">
            <span className="text-ink-500">书签</span>
            <span className="text-mist-300 tabular-nums">{bookmarkCount} 个</span>
          </div>
          <div className="flex justify-between">
            <span className="text-ink-500">当前时间</span>
            <button
              className="text-cyan-accent tabular-nums hover:underline"
              onClick={() => onSeek(Math.max(0, currentTime - 15))}
              title="点一下回到 15 秒前"
            >
              {formatMsTime(currentTime * 1000)}
            </button>
          </div>
          {!modelsReady && (
            <div className="text-st-warn pt-1">转写模型未就绪，请到设置页下载</div>
          )}
          {error && <div className="text-st-err pt-1">{error}</div>}
        </div>
      </div>
    </aside>
  );
}
