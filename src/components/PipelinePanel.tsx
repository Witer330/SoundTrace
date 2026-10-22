import { useCallback, useEffect, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  countEditedSegments,
  cancelDiarize,
  diarizeStatus,
  ensurePeaks,
  errMsg,
  generateSummary,
  getSettings,
  listBookmarks,
  listSegments,
  modelsStatus,
  runDiarize,
  setSettings,
  transcribeRecording,
} from "../api";
import type { DiarEvent, JobRow } from "../api";
import { formatBytes, formatDateTime, formatDuration, formatMsTime, type Recording } from "../types";
import { useAppStore } from "../store";
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
  extra,
  action,
}: {
  name: string;
  state: StageState;
  lines: { label: string; value: string; tone?: string }[];
  /** 阶段内联控件（如人数选择），渲染在产物行与操作按钮之间 */
  extra?: ReactNode;
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
      {extra && <div className="mt-1.5 flex items-center gap-1.5">{extra}</div>}
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
  const [busy, setBusy] = useState<"" | "peaks" | "transcribe">("");
  const [error, setError] = useState("");
  // 说话人自动分离：人数选择（锁人数是产品路径，自动阈值实测不可用）与运行状态
  const [diarCount, setDiarCount] = useState(2);
  const [diarBusy, setDiarBusy] = useState(false);
  const [diarProgress, setDiarProgress] = useState(0);
  // 分离模型是否就绪（与转写模型相互独立）
  const [spkModelsReady, setSpkModelsReady] = useState(true);
  // 纪要生成是后台线程，状态记在全局 store：切页签/重进详情都不会丢
  const summarizing = useAppStore((s) => s.summarizingId === rec.id);
  const setSummarizingId = useAppStore((s) => s.setSummarizingId);

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
      .then((ms) => {
        setModelsReady(ms.filter((m) => ["paraformer", "vad", "punct"].includes(m.id)).every((m) => m.ready));
        setSpkModelsReady(
          ms.filter((m) => ["seg", "campplus"].includes(m.id)).every((m) => m.ready),
        );
      })
      .catch(() => undefined);
  }, [load, rec.status]);

  // 分离人数预填：优先用上次的选择，否则按参会人字段估算（有人没发言时允许改）
  useEffect(() => {
    getSettings()
      .then((s) => {
        const saved = s[`diar.clusters.${rec.id}`];
        if (saved && Number(saved) >= 1) {
          setDiarCount(Math.min(8, Number(saved)));
        } else {
          const n = rec.participants
            .split(/[、,，;；/·]+/)
            .map((t) => t.trim())
            .filter(Boolean).length;
          setDiarCount(Math.min(8, Math.max(2, n)));
        }
      })
      .catch(() => undefined);
  }, [rec.id, rec.participants]);

  // 分离进度事件：running 更新进度，终态刷新流水线
  useEffect(() => {
    const un = listen<DiarEvent>("diar://progress", (e) => {
      const p = e.payload;
      if (p.recordingId !== rec.id) return;
      if (p.status === "running") {
        setDiarBusy(true);
        setDiarProgress(p.progress);
      } else {
        setDiarBusy(false);
        setDiarProgress(0);
        if (p.status === "failed") setError(p.error || "分离失败");
        if (p.status === "done") {
          load();
          onChanged();
        }
      }
    });
    return () => {
      void un.then((fn) => fn());
    };
  }, [rec.id, load, onChanged]);

  // 挂载时向后端核实运行状态：进度记在后端槽里，切走页面再回来不丢「进行中」
  useEffect(() => {
    let alive = true;
    diarizeStatus()
      .then((s) => {
        if (!alive) return;
        if (s.recordingId === rec.id) {
          setDiarBusy(true);
          setDiarProgress(s.progress);
        }
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [rec.id]);

  // 纪要生成结果（全局收尾在 App：清 store 状态 + 刷新列表；这里负责本页反馈）
  useEffect(() => {
    const un = listen<{ recordingId: number; ok: boolean; error: string }>(
      "summarize://done",
      (e) => {
        if (e.payload.recordingId !== rec.id) return;
        if (e.payload.ok) onChanged();
        else setError(e.payload.error || "纪要生成失败");
      },
    );
    return () => {
      void un.then((fn) => fn());
    };
  }, [rec.id, onChanged]);

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
    if (summarizing) return;
    setError("");
    setSummarizingId(rec.id);
    generateSummary(rec.id).catch((e) => {
      setSummarizingId(null);
      setError(errMsg(e));
    });
  };

  const runDiarizeNow = () => {
    setError("");
    runDiarize(rec.id, diarCount).catch((e) => setError(errMsg(e)));
  };

  const cancelDiarizeNow = () => {
    cancelDiarize(rec.id).catch((e) => setError(errMsg(e)));
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

  const summaryState: StageState = summarizing
    ? "running"
    : rec.summaryMd
      ? "done"
      : rec.status === "done"
        ? "idle"
        : "pending";

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
                      value: `约 ${formatDuration(
                        Math.max(
                          0,
                          (rec.durationSec * (1 - (job?.progress ?? 0))) / 8,
                        ),
                      )}（按 8 倍速估算）`,
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
            name="说话人分离"
            state={diarBusy ? "running" : speakerCount > 0 ? "done" : "idle"}
            lines={
              diarBusy
                ? [
                    {
                      label: "进度",
                      value:
                        diarProgress > 0.25
                          ? `${Math.round(diarProgress * 100)}%`
                          : "整段推理中，无精确进度…",
                      tone: "text-st-info",
                    },
                  ]
                : rec.status !== "done"
                  ? [
                      { label: "预设", value: `${diarCount} 人` },
                      { label: "说明", value: "转写完成后自动分离", tone: "text-ink-500" },
                    ]
                  : speakerTotal === 0
                    ? [{ label: "状态", value: "未标注", tone: "text-ink-500" }]
                    : [
                        { label: "说话人", value: `${speakerCount} 人` },
                        { label: "已标注", value: `${speakerTotal} 段` },
                        { label: "保护", value: "手工标注不被覆盖" },
                      ]
            }
            extra={
              <label className="flex items-center gap-1 text-[11px] text-ink-500">
                人数
                <select
                  className="text-xs py-0.5"
                  value={diarCount}
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    setDiarCount(n);
                    // 选择即持久化：转写完成前预设的人数，转写收尾会自动接分离
                    setSettings({ [`diar.clusters.${rec.id}`]: String(n) }).catch(() => undefined);
                  }}
                  disabled={diarBusy}
                  title="分错时改人数重跑；默认按参会人预填；转写完成前设定则转写完自动分离"
                >
                  {[2, 3, 4, 5, 6, 7, 8].map((n) => (
                    <option key={n} value={n}>
                      {n}
                    </option>
                  ))}
                </select>
              </label>
            }
            action={
              diarBusy
                ? { label: "取消分离", onClick: cancelDiarizeNow, busy: false }
                : rec.status === "done"
                  ? {
                      label: speakerTotal > 0 ? "重新分离" : "自动分离",
                      onClick: runDiarizeNow,
                      disabled: !spkModelsReady,
                    }
                  : undefined
            }
          />

          <Stage
            name="AI 纪要"
            state={summaryState}
            lines={
              summarizing
                ? [{ label: "状态", value: "正在生成（需 1~3 分钟）…", tone: "text-st-info" }]
                : rec.summaryMd
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
                    busy: summarizing,
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
          {!spkModelsReady && (
            <div className="text-st-warn pt-1">说话人模型未就绪，请到设置页下载</div>
          )}
          {error && <div className="text-st-err pt-1">{error}</div>}
        </div>
      </div>
    </aside>
  );
}
