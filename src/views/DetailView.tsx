import { useEffect, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { deleteRecording, errMsg, getRecording, updateRecording } from "../api";
import { useAppStore } from "../store";
import {
  formatBytes,
  formatDateTime,
  formatDuration,
  STATUS_LABEL,
  type Recording,
} from "../types";

export default function DetailView({ id }: { id: number }) {
  const go = useAppStore((s) => s.go);
  const [rec, setRec] = useState<Recording | null>(null);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [savedFlash, setSavedFlash] = useState(false);

  // 编辑态
  const [title, setTitle] = useState("");
  const [participants, setParticipants] = useState("");
  const [tagsText, setTagsText] = useState("");
  const [notes, setNotes] = useState("");

  useEffect(() => {
    getRecording(id)
      .then((r) => {
        if (r) {
          setRec(r);
          setTitle(r.title);
          setParticipants(r.participants);
          setTagsText(r.tags.join(" "));
          setNotes(r.notes);
        } else {
          setError("录音不存在（可能已被删除）");
        }
      })
      .catch((e) => setError(errMsg(e)));
  }, [id]);

  async function save() {
    if (!rec) return;
    setSaving(true);
    try {
      const updated = await updateRecording(id, {
        title: title.trim() || rec.title,
        participants,
        notes,
        tags: tagsText.split(/[\s,，;；]+/).filter(Boolean),
      });
      if (updated) setRec(updated);
      setSavedFlash(true);
      setTimeout(() => setSavedFlash(false), 1500);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setSaving(false);
    }
  }

  async function doDelete() {
    if (!rec) return;
    const msg = `确定删除「${rec.title}」吗？\n\n“删除”仅移除库记录（保留归档文件）；“彻底删除”同时删除音频文件，不可恢复。`;
    // TODO: 换成正式对话框组件；先用 confirm 快速实现
    const choice = window.confirm(msg);
    if (!choice) return;
    const wipe = window.confirm("是否同时删除归档的音频文件？（取消 = 仅移除库记录）");
    try {
      await deleteRecording(id, wipe);
      go({ kind: "library" });
    } catch (e) {
      setError(errMsg(e));
    }
  }

  if (error) {
    return (
      <div className="h-full flex flex-col items-center justify-center gap-3">
        <div className="text-red-300 text-sm">{error}</div>
        <button className="text-mist-400 hover:text-mist-200 text-sm" onClick={() => go({ kind: "library" })}>
          返回录音库
        </button>
      </div>
    );
  }

  if (!rec) {
    return <div className="h-full flex items-center justify-center text-mist-400 text-sm">加载中…</div>;
  }

  return (
    <div className="h-full flex flex-col">
      {/* 头部 */}
      <div className="flex items-center gap-3 px-4 h-14 border-b border-ink-800 bg-ink-900/60">
        <button
          className="text-mist-400 hover:text-mist-200 text-sm"
          onClick={() => go({ kind: "library" })}
        >
          ← 返回
        </button>
        <div className="flex-1 truncate text-mist-200">{rec.title}</div>
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200"
          onClick={() => void revealItemInDir(rec.filePath).catch((e) => setError(errMsg(e)))}
        >
          打开所在位置
        </button>
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200"
          onClick={save}
          disabled={saving}
        >
          {savedFlash ? "已保存 ✓" : saving ? "保存中…" : "保存"}
        </button>
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-red-900/60 hover:bg-red-800/70 text-red-200"
          onClick={doDelete}
        >
          删除
        </button>
      </div>

      <div className="flex-1 overflow-y-auto p-4 grid grid-cols-[1fr_320px] gap-4">
        {/* 左列：播放器（M2）+ 转写（M3）占位 */}
        <div className="space-y-4">
          <div className="rounded-xl border border-ink-800 bg-ink-900 p-4">
            <div className="text-mist-300 text-sm font-medium mb-2">播放器</div>
            <div className="text-mist-400 text-xs">
              波形播放器将在 M2 里程碑提供（倍速 / 快进快退 / 书签 / 点字跳转）。
            </div>
          </div>
          <div className="rounded-xl border border-ink-800 bg-ink-900 p-4 min-h-[240px]">
            <div className="text-mist-300 text-sm font-medium mb-2">转写稿</div>
            <div className="text-mist-400 text-xs">
              转写功能将在 M3 里程碑提供（本地离线 · Paraformer）。
            </div>
          </div>
        </div>

        {/* 右列：元数据 */}
        <div className="space-y-4">
          <div className="rounded-xl border border-ink-800 bg-ink-900 p-4 space-y-3">
            <div className="text-mist-300 text-sm font-medium">信息</div>
            <dl className="text-xs space-y-1.5">
              <div className="flex justify-between gap-2">
                <dt className="text-ink-500">状态</dt>
                <dd className="text-mist-300">{STATUS_LABEL[rec.status]}</dd>
              </div>
              <div className="flex justify-between gap-2">
                <dt className="text-ink-500">时长</dt>
                <dd className="text-mist-300">{formatDuration(rec.durationSec)}</dd>
              </div>
              <div className="flex justify-between gap-2">
                <dt className="text-ink-500">大小</dt>
                <dd className="text-mist-300">{formatBytes(rec.sizeBytes)}</dd>
              </div>
              <div className="flex justify-between gap-2">
                <dt className="text-ink-500">格式</dt>
                <dd className="text-mist-300 uppercase">{rec.format}</dd>
              </div>
              <div className="flex justify-between gap-2">
                <dt className="text-ink-500">导入于</dt>
                <dd className="text-mist-300">{formatDateTime(rec.createdAt)}</dd>
              </div>
              <div className="flex justify-between gap-2">
                <dt className="text-ink-500">原始文件</dt>
                <dd className="text-mist-300 truncate max-w-[160px]" title={rec.origName}>
                  {rec.origName}
                </dd>
              </div>
            </dl>
          </div>

          <div className="rounded-xl border border-ink-800 bg-ink-900 p-4 space-y-3">
            <div className="text-mist-300 text-sm font-medium">元数据</div>
            <label className="block space-y-1">
              <span className="text-xs text-ink-500">标题</span>
              <input
                type="text"
                className="w-full"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
              />
            </label>
            <label className="block space-y-1">
              <span className="text-xs text-ink-500">参会人（逗号或空格分隔）</span>
              <input
                type="text"
                className="w-full"
                value={participants}
                onChange={(e) => setParticipants(e.target.value)}
                placeholder="张三 李四 王五"
              />
            </label>
            <label className="block space-y-1">
              <span className="text-xs text-ink-500">标签（空格分隔）</span>
              <input
                type="text"
                className="w-full"
                value={tagsText}
                onChange={(e) => setTagsText(e.target.value)}
                placeholder="周会 项目A"
              />
            </label>
            <label className="block space-y-1">
              <span className="text-xs text-ink-500">笔记</span>
              <textarea
                className="w-full min-h-[90px] resize-y"
                value={notes}
                onChange={(e) => setNotes(e.target.value)}
                placeholder="会议背景、待办提醒…"
              />
            </label>
          </div>
        </div>
      </div>
    </div>
  );
}
