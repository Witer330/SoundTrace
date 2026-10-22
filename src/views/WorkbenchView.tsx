import { useState } from "react";
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
import { formatDuration, type Recording } from "../types";
import StatusBadge from "../components/StatusBadge";
import DropMask from "../components/DropMask";
import { useAudioDrop } from "../hooks/useAudioDrop";

/** 累计音频时长的口语化格式：68 小时 20 分 */
function formatTotalDuration(sec: number): string {
  if (sec <= 0) return "0 分钟";
  const h = Math.floor(sec / 3600);
  const m = Math.round((sec % 3600) / 60);
  if (h === 0) return `${m} 分钟`;
  return m === 0 ? `${h} 小时` : `${h} 小时 ${m} 分`;
}

function formatRelative(iso: string): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const diff = Date.now() - t;
  const min = Math.floor(diff / 60000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h} 小时前`;
  return `${Math.floor(h / 24)} 天前`;
}

const JOB_LABEL: Record<JobRow["status"], string> = {
  pending: "排队中",
  running: "转写中",
  done: "已完成",
  failed: "失败",
  canceled: "已取消",
};

function JobStatusDot({ status }: { status: JobRow["status"] }) {
  if (status === "running") {
    return (
      <span className="w-2 h-2 rounded-full bg-st-info animate-pulse shrink-0" title="转写中" />
    );
  }
  if (status === "pending") {
    return <span className="w-2 h-2 rounded-full bg-st-warn shrink-0" title="排队中" />;
  }
  if (status === "done") {
    return <span className="w-2 h-2 rounded-full bg-st-ok shrink-0" title="已完成" />;
  }
  if (status === "failed") {
    return <span className="w-2 h-2 rounded-full bg-st-err shrink-0" title="失败" />;
  }
  return <span className="w-2 h-2 rounded-full bg-st-muted shrink-0" title="已取消" />;
}

function JobCard({ job, queueIndex }: { job: JobRow; queueIndex: number }) {
  const go = useAppStore((s) => s.go);
  const recordings = useAppStore((s) => s.recordings);
  const refreshAll = useAppStore((s) => s.refreshAll);
  const [busy, setBusy] = useState(false);
  const [retryErr, setRetryErr] = useState("");
  const rec = job.recordingId ? recordings.find((r) => r.id === job.recordingId) : undefined;
  const active = job.status === "running" || job.status === "pending";

  const meta =
    job.status === "running"
      ? `${Math.round(job.progress * 100)}%`
      : job.status === "pending"
        ? `队列第 ${queueIndex} 位`
        : job.status === "failed"
          ? (retryErr || job.error || "转写失败")
          : formatRelative(job.updatedAt);

  return (
    <div className="flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-ink-850 transition-colors">
      <JobStatusDot status={job.status} />
      <div className="w-12 shrink-0 text-[11px] text-ink-500">{JOB_LABEL[job.status]}</div>

      <button
        className="flex-1 min-w-0 text-left group"
        onClick={() =>
          job.recordingId && go({ kind: "detail", id: job.recordingId, from: "workbench" })
        }
        disabled={!job.recordingId}
      >
        <div className="text-sm text-mist-200 truncate group-hover:text-cyan-accent">
          {job.title ?? "（录音已删除）"}
        </div>
        <div className="text-[11px] text-ink-500 flex items-center gap-2 mt-0.5">
          <span className="tabular-nums">{formatDuration(job.durationSec)}</span>
          <span className={job.status === "failed" ? "text-st-err truncate" : ""}>{meta}</span>
        </div>
      </button>

      {job.status === "running" && (
        <div className="w-32 shrink-0 h-1.5 rounded-full bg-ink-800 overflow-hidden">
          <div
            className="h-full bg-gradient-to-r from-teal-accent to-cyan-accent transition-all"
            style={{ width: `${Math.round(job.progress * 100)}%` }}
          />
        </div>
      )}

      {active && (
        <button
          className="shrink-0 px-2 py-1 rounded text-[11px] bg-ink-800 hover:bg-ink-700 text-mist-400 hover:text-mist-200 disabled:opacity-50"
          disabled={busy || !job.recordingId}
          onClick={async () => {
            if (!job.recordingId) return;
            setBusy(true);
            try {
              await cancelTranscribe(job.recordingId);
              await refreshAll();
            } catch {
              // 忽略：状态会由事件回推
            } finally {
              setBusy(false);
            }
          }}
        >
          取消
        </button>
      )}

      {job.status === "failed" && job.recordingId && (
        <button
          className="shrink-0 px-2 py-1 rounded text-[11px] bg-ink-800 hover:bg-ink-700 text-mist-400 hover:text-mist-200 disabled:opacity-50"
          disabled={busy}
          title="重新排队转写"
          onClick={async () => {
            setRetryErr("");
            setBusy(true);
            try {
              await transcribeRecording(job.recordingId!);
              await refreshAll();
            } catch (e) {
              setRetryErr(errMsg(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          重试
        </button>
      )}

      {job.status === "done" && rec?.summaryMd && (
        <span className="shrink-0 text-[11px] text-st-ok">有纪要</span>
      )}
    </div>
  );
}

/** 统计卡：大数字 + 标签 + 图标（对齐参考稿的首页卡片） */
function StatCard({
  value,
  unit,
  label,
  tone,
  icon,
}: {
  value: string;
  unit?: string;
  label: string;
  tone: string;
  icon: React.ReactNode;
}) {
  return (
    <div className="rounded-xl border border-ink-800 bg-ink-900 px-4 py-3.5 flex items-center gap-3">
      <div className={`w-9 h-9 rounded-lg grid place-items-center shrink-0 ${tone}`}>{icon}</div>
      <div className="min-w-0">
        <div className="text-xl font-semibold text-mist-100 tabular-nums leading-tight truncate">
          {value}
          {unit && <span className="text-sm font-normal text-mist-400 ml-0.5">{unit}</span>}
        </div>
        <div className="text-[11px] text-ink-500 mt-0.5">{label}</div>
      </div>
    </div>
  );
}

export default function WorkbenchView() {
  const recordings = useAppStore((s) => s.recordings);
  const loaded = useAppStore((s) => s.recordingsLoaded);
  const jobs = useAppStore((s) => s.jobs);
  const lastResult = useAppStore((s) => s.lastImport);
  const setLastResult = useAppStore((s) => s.setLastImport);
  const go = useAppStore((s) => s.go);
  const refreshAll = useAppStore((s) => s.refreshAll);

  const [importing, setImporting] = useState(false);

  async function doImport(paths: string[] | null) {
    if (!paths || paths.length === 0) return;
    setImporting(true);
    try {
      const result = await importPaths(paths);
      setLastResult(result);
      await refreshAll();
      // 跳到录音库看新条目；提示条读的是 store 里的 lastImport，跳过去仍能看到
      if (result.imported.length > 0) go({ kind: "library" });
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
      return; // 对话框异常（极少见），不提示
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

  // ---- 统计 ----
  const weekAgo = Date.now() - 7 * 24 * 3600 * 1000;
  const doneThisWeek = jobs.filter(
    (j) => j.status === "done" && new Date(j.updatedAt).getTime() >= weekAgo,
  ).length;
  const totalDuration = recordings.reduce((a, r) => a + (r.durationSec || 0), 0);
  const activeJobs = jobs.filter((j) => j.status === "running" || j.status === "pending");
  const recentJobs = jobs
    .filter((j) => j.status !== "running" && j.status !== "pending")
    .slice(0, 4);
  const queueJobs = [...activeJobs, ...recentJobs];
  const recentRecordings = [...recordings]
    .sort((a, b) => (b.recordedAt ?? b.createdAt).localeCompare(a.recordedAt ?? a.createdAt))
    .slice(0, 6);

  const pendingCount = activeJobs.length;
  // 队列位次：有正在跑的任务时它占第 1 位，pending 从 2 起算；空闲时 pending 从 1 起算
  const hasRunning = activeJobs.some((j) => j.status === "running");
  const runningIndex = new Map<number, number>();
  activeJobs
    .filter((j) => j.status === "pending")
    .forEach((j, i) => runningIndex.set(j.id, i + (hasRunning ? 2 : 1)));

  return (
    <div className="h-full relative">
      <div className="h-full overflow-y-auto">
      <div className="max-w-5xl mx-auto p-6 space-y-5">
        {/* 头部 */}
        <div className="flex items-start gap-3">
          <div className="min-w-0">
            <h1 className="text-xl font-semibold text-mist-100">工作台</h1>
            <p className="text-xs text-mist-400 mt-1">
              导入手机录音 → 本地离线转写 → 点字跳音频 → 全文检索 → AI 会议纪要
            </p>
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

        {lastResult && (
          <div className="px-3 py-2 rounded-lg bg-ink-850 border border-ink-700 text-sm flex items-start gap-2">
            <div className="flex-1 text-mist-300">
              导入完成：新增 {lastResult.imported.length} 条
              {lastResult.skipped.length > 0 && (
                <span className="text-mist-400">
                  {" "}
                  · 跳过 {lastResult.skipped.length} 条（
                  {lastResult.skipped.map((s) => s.reason).join("；").slice(0, 100)}）
                </span>
              )}
            </div>
            <button className="text-ink-500 hover:text-mist-300" onClick={() => setLastResult(null)}>
              ✕
            </button>
          </div>
        )}

        {/* 统计卡 */}
        <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
          <StatCard
            value={String(recordings.length)}
            unit="条"
            label="录音总数"
            tone="bg-ink-800 text-cyan-accent"
            icon={
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
                <path d="M4 14v-4M8 17V7M12 20V4M16 17V7M20 14v-4" />
              </svg>
            }
          />
          <StatCard
            value={pendingCount === 0 ? "0" : String(pendingCount)}
            unit="条"
            label={pendingCount === 0 ? "待处理（空闲）" : "待处理"}
            tone="bg-st-warn-soft text-st-warn"
            icon={
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
                <circle cx="12" cy="12" r="8.5" />
                <path d="M12 8v4.5l3 1.8" />
              </svg>
            }
          />
          <StatCard
            value={String(doneThisWeek)}
            unit="条"
            label="本周完成转写"
            tone="bg-st-ok-soft text-st-ok"
            icon={
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                <path d="M20 6 9 17l-5-5" />
              </svg>
            }
          />
          <StatCard
            value={formatTotalDuration(totalDuration)}
            label="累计音频时长"
            tone="bg-st-info-soft text-st-info"
            icon={
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
                <path d="M12 3v18M6.5 7v10M17.5 7v10M3 10.5v3M21 10.5v3" />
              </svg>
            }
          />
        </div>

        {/* 处理队列 */}
        <section className="rounded-xl border border-ink-800 bg-ink-900">
          <div className="flex items-center gap-2 px-4 py-3 border-b border-ink-800">
            <h2 className="text-sm font-medium text-mist-300">处理队列</h2>
            {activeJobs.length > 0 && (
              <span className="text-[11px] text-ink-500">
                {activeJobs.filter((j) => j.status === "running").length} 条进行中 ·{" "}
                {activeJobs.filter((j) => j.status === "pending").length} 条排队
              </span>
            )}
            <div className="flex-1" />
            <button
              className="text-xs text-mist-400 hover:text-cyan-accent"
              onClick={() => go({ kind: "library" })}
            >
              去录音库排队 →
            </button>
          </div>
          <div className="p-1.5">
            {!loaded ? (
              <div className="px-3 py-6 text-center text-sm text-mist-400">加载中…</div>
            ) : queueJobs.length === 0 ? (
              <div className="px-3 py-8 text-center">
                <div className="text-sm text-mist-400">队列是空的</div>
                <div className="text-xs text-ink-500 mt-1">
                  在录音库里勾选录音 → 批量转写，任务会在这里排队显示
                </div>
              </div>
            ) : (
              queueJobs.map((j) => (
                <JobCard key={j.id} job={j} queueIndex={runningIndex.get(j.id) ?? 1} />
              ))
            )}
          </div>
        </section>

        {/* 最近录音 */}
        <section>
          <div className="flex items-center gap-2 mb-2.5 px-1">
            <h2 className="text-sm font-medium text-mist-300">最近录音</h2>
            <div className="flex-1" />
            <button
              className="text-xs text-mist-400 hover:text-cyan-accent"
              onClick={() => go({ kind: "library" })}
            >
              全部录音 →
            </button>
          </div>
          {recentRecordings.length === 0 ? (
            <div className="rounded-xl border border-dashed border-ink-700 px-4 py-8 text-center">
              <div className="text-sm text-mist-300">录音库还是空的</div>
              <div className="text-xs text-mist-400 mt-1">
                点右上角「导入文件夹」，把手机录音整个目录拖进来也可以
              </div>
            </div>
          ) : (
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-2">
              {recentRecordings.map((r: Recording) => (
                <button
                  key={r.id}
                  onClick={() => go({ kind: "detail", id: r.id, from: "workbench" })}
                  className="flex items-center gap-3 px-4 py-3 rounded-xl bg-ink-900 border border-ink-800 hover:border-ink-600 hover:bg-ink-850 transition-colors text-left"
                >
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="text-sm text-mist-200 truncate">{r.title}</span>
                      <StatusBadge status={r.status} />
                    </div>
                    <div className="text-[11px] text-ink-500 mt-1 flex gap-3 flex-wrap">
                      <span className="tabular-nums">{formatDuration(r.durationSec)}</span>
                      {r.summaryMd && <span className="text-st-ok">有纪要</span>}
                      {r.tags.slice(0, 3).map((t) => (
                        <span key={t}>{t}</span>
                      ))}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </section>

        {!isTauri() && (
          <div className="text-[11px] text-ink-500 text-center pt-2">
            浏览器调试模式：无本地后端，导入与转写不可用
          </div>
        )}
      </div>
      </div>
      {dragging && <DropMask />}
    </div>
  );
}
