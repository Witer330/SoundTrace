export interface AppInfo {
  version: string;
  dataRoot: string;
  defaultDataRoot: string;
}

export type RecordingStatus =
  | "imported"
  | "queued"
  | "transcribing"
  | "done"
  | "failed";

export interface Recording {
  id: number;
  fileHash: string;
  filePath: string;
  origName: string;
  title: string;
  recordedAt: string | null;
  durationSec: number;
  sizeBytes: number;
  format: string;
  status: RecordingStatus;
  notes: string;
  participants: string;
  summaryMd: string | null;
  llmModel: string | null;
  createdAt: string;
  updatedAt: string;
  tags: string[];
}

export interface Segment {
  id: number;
  recordingId: number;
  startMs: number;
  endMs: number;
  text: string;
  speaker: number | null;
}

export interface Bookmark {
  id: number;
  recordingId: number;
  timeMs: number;
  label: string;
  note: string;
}

export type JobKind = "transcribe" | "summarize" | "peaks";
export type JobStatus = "pending" | "running" | "done" | "failed" | "canceled";

export interface Job {
  id: number;
  recordingId: number | null;
  kind: JobKind;
  status: JobStatus;
  progress: number;
  error: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface ImportResult {
  imported: Recording[];
  skipped: { path: string; reason: string }[];
}

export interface RecordingPatch {
  title?: string;
  notes?: string;
  participants?: string;
  recordedAt?: string;
  tags?: string[];
}

export type View =
  | { kind: "library" }
  | { kind: "detail"; id: number }
  | { kind: "settings" };

export function formatDuration(sec: number): string {
  if (!sec || sec <= 0) return "--:--";
  const s = Math.floor(sec % 60);
  const m = Math.floor((sec / 60) % 60);
  const h = Math.floor(sec / 3600);
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${m}:${ss}`;
}

export function formatMsTime(ms: number): string {
  const sec = Math.floor(ms / 1000);
  const s = sec % 60;
  const m = Math.floor(sec / 60) % 60;
  const h = Math.floor(sec / 3600);
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

export function formatBytes(bytes: number): string {
  if (!bytes || bytes <= 0) return "-";
  const units = ["B", "KB", "MB", "GB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 || i === 0 ? 0 : 1)} ${units[i]}`;
}

export function formatDateTime(iso: string | null): string {
  if (!iso) return "-";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(
    d.getHours(),
  )}:${pad(d.getMinutes())}`;
}

export function formatDate(iso: string | null): string {
  if (!iso) return "-";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export const STATUS_LABEL: Record<RecordingStatus, string> = {
  imported: "已导入",
  queued: "排队中",
  transcribing: "转写中",
  done: "已转写",
  failed: "转写失败",
};
