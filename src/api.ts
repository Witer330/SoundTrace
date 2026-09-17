import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  Bookmark,
  ImportResult,
  Recording,
  RecordingPatch,
} from "./types";

/** 是否运行在 Tauri 壳内（纯浏览器调试时为 false，跳过原生能力） */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function getAppInfo(): Promise<AppInfo> {
  return invoke("get_app_info");
}

export async function getSettings(): Promise<Record<string, string>> {
  return invoke("get_settings");
}

export async function setSettings(
  entries: Record<string, string>,
): Promise<void> {
  await invoke("set_settings", { entries });
}

export async function changeDataRoot(newRoot: string): Promise<void> {
  await invoke("change_data_root", { newRoot });
}

// ---------- 录音库（M1） ----------

export async function listRecordings(): Promise<Recording[]> {
  return invoke("list_recordings");
}

export async function getRecording(id: number): Promise<Recording | null> {
  return invoke("get_recording", { id });
}

export async function importFiles(paths: string[]): Promise<ImportResult> {
  return invoke("import_files", { paths });
}

/** 展开（目录递归）为音频文件列表 */
export async function expandAudioPaths(paths: string[]): Promise<string[]> {
  return invoke("expand_audio_paths", { paths });
}

/** 导入一组文件/目录路径：先展开再导入 */
export async function importPaths(paths: string[]): Promise<ImportResult> {
  const expanded = await expandAudioPaths(paths);
  if (expanded.length === 0) {
    return { imported: [], skipped: [{ path: paths.join("; "), reason: "未找到音频文件" }] };
  }
  return importFiles(expanded);
}

export async function updateRecording(
  id: number,
  patch: RecordingPatch,
): Promise<Recording | null> {
  return invoke("update_recording", { id, patch });
}

export async function deleteRecording(
  id: number,
  deleteFile: boolean,
): Promise<void> {
  await invoke("delete_recording", { id, deleteFile });
}

// ---------- 播放器（M2） ----------

export async function ensurePeaks(
  id: number,
): Promise<{ peaks: number[]; durationSec: number }> {
  return invoke("ensure_peaks", { id });
}

export async function listBookmarks(recordingId: number): Promise<Bookmark[]> {
  return invoke("list_bookmarks", { recordingId });
}

export async function addBookmark(
  recordingId: number,
  timeMs: number,
  label: string,
  note = "",
): Promise<Bookmark> {
  return invoke("add_bookmark", { recordingId, timeMs, label, note });
}

export async function deleteBookmark(id: number): Promise<void> {
  await invoke("delete_bookmark", { id });
}

// ---------- 转写（M3） ----------

export interface ModelStatus {
  id: string;
  name: string;
  ready: boolean;
  presentBytes: number;
  totalBytes: number;
}

export async function modelsStatus(): Promise<ModelStatus[]> {
  return invoke("models_status");
}

export async function downloadModels(ids: string[]): Promise<void> {
  await invoke("download_models", { ids });
}

export interface TranscriptSegment {
  id: number;
  recordingId: number;
  startMs: number;
  endMs: number;
  text: string;
  /** JSON: [[char, ms], ...] */
  chars: string;
}

export async function listSegments(recordingId: number): Promise<TranscriptSegment[]> {
  return invoke("list_segments", { recordingId });
}

export async function transcribeRecording(recordingId: number): Promise<number> {
  return invoke("transcribe", { recordingId });
}

export async function cancelTranscribe(recordingId: number): Promise<void> {
  await invoke("cancel_transcribe", { recordingId });
}

export interface JobEvent {
  recordingId: number;
  status: "queued" | "running" | "done" | "failed" | "canceled";
  progress: number;
  error: string;
}

export interface ModelDownloadProgress {
  modelId: string;
  bytes: number;
  total: number;
  file: string;
}

// ---------- 检索 / AI 复盘 / 导出（M4） ----------

export interface SearchResults {
  recordings: {
    id: number;
    title: string;
    recordedAt: string | null;
    durationSec: number;
    status: string;
    tags: string[];
  }[];
  segments: {
    recordingId: number;
    recordingTitle: string;
    recordedAt: string | null;
    startMs: number;
    snippet: string;
  }[];
}

export async function searchAll(query: string): Promise<SearchResults> {
  return invoke("search_all", { query });
}

export async function generateSummary(recordingId: number): Promise<void> {
  await invoke("generate_summary", { recordingId });
}

export async function testLlm(): Promise<string> {
  return invoke("test_llm");
}

export async function exportRecording(
  recordingId: number,
  kind: "md" | "srt" | "txt",
  dest: string,
): Promise<void> {
  await invoke("export_recording", { recordingId, kind, dest });
}

/** 后端错误统一为 { msg }，提取可读文本 */
export function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "msg" in e) return String((e as { msg: unknown }).msg);
  return String(e);
}
