export interface AppInfo {
  version: string;
  dataRoot: string;
  defaultDataRoot: string;
  /** 逻辑核心数（含超线程） */
  logicalCores: number;
  /** 「自动」档实际使用的线程数（物理核心数，至少留 2 个逻辑核心） */
  autoThreads: number;
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

export type ListViewKind = "workbench" | "library" | "search";

export type View =
  | { kind: "workbench" }
  | { kind: "library" }
  | { kind: "search" }
  | { kind: "detail"; id: number; seekMs?: number; from?: ListViewKind }
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

/** 纪要首行「> 类型 · 主题」的解析结果；kind 为空串表示缺分隔符 */
export interface SummaryHeadline {
  kind: string;
  topic: string;
}

/** 解析纪要首行「> 类型 · 主题」，供列表类型徽章使用（与后端 parse_headline 同规则）：
 *  首个非空行必须是引用块，分隔符宽容 ·/・/•，缺分隔符时整行当主题 */
export function parseSummaryHeadline(md: string | null): SummaryHeadline | null {
  if (!md) return null;
  const line = md.split("\n").map((l) => l.trim()).find((l) => l !== "");
  if (!line || !line.startsWith(">")) return null;
  const body = line.slice(1).trim();
  if (!body) return null;
  const strip = (s: string) =>
    s.replace(/^[*_\s]+/, "").replace(/[*_\s]+$/, "").trim();
  const sep = body.search(/·|・|•/);
  if (sep < 0) return { kind: "", topic: strip(body) };
  const kind = strip(body.slice(0, sep));
  const topic = strip(body.slice(sep + 1));
  return topic ? { kind, topic } : null;
}
