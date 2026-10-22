import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import PlayerBar, { SPEEDS } from "../components/PlayerBar";
import TranscriptPanel from "../components/TranscriptPanel";
import SummaryPanel from "../components/SummaryPanel";
import PipelinePanel from "../components/PipelinePanel";
import {
  addBookmark,
  deleteBookmark,
  deleteRecording,
  ensurePeaks,
  errMsg,
  exportRecording,
  getRecording,
  isTauri,
  listBookmarks,
  listSegments,
  updateRecording,
} from "../api";
import { useAppStore } from "../store";
import {
  formatBytes,
  formatDateTime,
  formatDuration,
  formatMsTime,
  STATUS_LABEL,
  type Bookmark,
  type ListViewKind,
  type Recording,
} from "../types";

type Tab = "transcript" | "summary" | "info";

const SPEED_KEY = "st.speed";

function readSavedSpeed(): number {
  try {
    const v = Number(localStorage.getItem(SPEED_KEY));
    return SPEEDS.includes(v) ? v : 1;
  } catch {
    return 1;
  }
}

export default function DetailView({
  id,
  seekMs,
  from,
}: {
  id: number;
  seekMs?: number;
  from?: ListViewKind;
}) {
  const go = useAppStore((s) => s.go);
  const jobs = useAppStore((s) => s.jobs);
  const [rec, setRec] = useState<Recording | null>(null);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [savedFlash, setSavedFlash] = useState(false);
  const [tab, setTab] = useState<Tab>("transcript");
  const [segCount, setSegCount] = useState(0);
  // 转写稿有未保存草稿时，切页签/返回前先确认
  const [transcriptDirty, setTranscriptDirty] = useState(false);

  // 播放器状态
  const audioRef = useRef<HTMLAudioElement>(null);
  const pendingSeekRef = useRef<number | null>(null);
  const [peaks, setPeaks] = useState<number[]>([]);
  const [peaksLoading, setPeaksLoading] = useState(true);
  const [currentTime, setCurrentTime] = useState(0);
  const [playing, setPlaying] = useState(false);
  // 倍速全局记住，播放位置按录音记住（localStorage）
  const [speed, setSpeed] = useState(readSavedSpeed);
  const [bookmarks, setBookmarks] = useState<Bookmark[]>([]);
  const posKey = `st.pos.${id}`;
  const lastPosSaveRef = useRef(0);

  // 编辑态
  const [title, setTitle] = useState("");
  const [participants, setParticipants] = useState("");
  const [tagsText, setTagsText] = useState("");
  const [notes, setNotes] = useState("");
  // 标题是否被用户改过：没改过就跟随后端（AI 纪要完成后主题会自动回填标题），改过则草稿优先
  const titleDirtyRef = useRef(false);

  const job = useMemo(
    () => jobs.find((j) => j.recordingId === id && (j.status === "running" || j.status === "pending")),
    [jobs, id],
  );

  const seek = useCallback((sec: number) => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = sec;
    setCurrentTime(sec);
  }, []);

  const refreshRec = useCallback(() => {
    getRecording(id)
      .then((r) => {
        if (r) setRec(r);
      })
      .catch(() => undefined);
  }, [id]);

  // 未被用户编辑过的标题跟随后端：纪要生成完成后主题自动回填，编辑框不能停在旧值，
  // 否则用户接着点「保存」会把旧标题写回去并误标记为手工命名
  useEffect(() => {
    if (rec && !titleDirtyRef.current) setTitle(rec.title);
  }, [rec]);

  /**
   * 详情页的数据入口。后端读不到时回退到 store 里已加载的行（列表数据通常够渲染），
   * 这样「后端短暂不可达」或纯浏览器预览时页面不会整屏报错。
   */
  const fallbackRec = useCallback(
    () => useAppStore.getState().recordings.find((r) => r.id === id),
    [id],
  );

  // 从全局搜索跳入时定位到命中时间点（等元数据就绪后执行）。
  // 依赖 peaksLoading：波形加载期间 PlayerBar 不渲染、audio 元素尚不存在，
  // 若只依赖 seekMs，这次定位会被静默丢掉（表现为「搜到的片段点进来没跳过去」）。
  useEffect(() => {
    if (seekMs === undefined) return;
    const audio = audioRef.current;
    if (audio && audio.readyState >= 1) {
      seek(seekMs / 1000);
    } else {
      pendingSeekRef.current = seekMs;
    }
  }, [seekMs, seek, peaksLoading]);

  function onAudioMetadata() {
    const pending = pendingSeekRef.current;
    if (pending !== null) {
      pendingSeekRef.current = null;
      seek(pending / 1000);
      return;
    }
    // 没有显式跳转时恢复上次播放位置（已听完或位置失效则从头开始）
    try {
      const saved = Number(localStorage.getItem(posKey));
      const dur = audioRef.current?.duration ?? 0;
      if (saved > 1000 && (!dur || Number.isNaN(dur) || saved < dur - 500)) seek(saved / 1000);
    } catch {
      // localStorage 不可用就当没有记忆
    }
  }

  /** 播放位置节流落盘（2 秒一次），暂停/关闭前的位置在下次 timeupdate 里已经存住 */
  const handleTimeUpdate = useCallback(
    (sec: number) => {
      setCurrentTime(sec);
      const now = Date.now();
      if (now - lastPosSaveRef.current >= 2000) {
        lastPosSaveRef.current = now;
        try {
          localStorage.setItem(posKey, String(Math.round(sec * 1000)));
        } catch {
          // 忽略
        }
      }
    },
    [posKey],
  );

  const changeSpeed = useCallback((s: number) => {
    setSpeed(s);
    try {
      localStorage.setItem(SPEED_KEY, String(s));
    } catch {
      // 忽略
    }
  }, []);

  const addBookmarkHere = useCallback(async () => {
    if (!rec) return;
    const label = window.prompt("书签名称：", `书签 ${formatMsTime(currentTime * 1000)}`);
    if (label === null) return;
    try {
      const bm = await addBookmark(rec.id, Math.round(currentTime * 1000), label.trim());
      setBookmarks((prev) => [...prev, bm].sort((a, b) => a.timeMs - b.timeMs));
    } catch (e) {
      setError(errMsg(e));
    }
  }, [rec, currentTime]);

  useEffect(() => {
    // 切换录音时重置标题草稿标记
    titleDirtyRef.current = false;
    const fallback = fallbackRec();
    getRecording(id)
      .then((r) => {
        const use = r ?? fallback;
        if (use) {
          setRec(use);
          setTitle(use.title);
          setParticipants(use.participants);
          setTagsText(use.tags.join(" "));
          setNotes(use.notes);
        } else {
          setError("录音不存在（可能已被删除）");
        }
      })
      .catch(() => {
        if (fallback) {
          setRec(fallback);
          setTitle(fallback.title);
          setParticipants(fallback.participants);
          setTagsText(fallback.tags.join(" "));
          setNotes(fallback.notes);
        } else {
          setError("无法读取录音信息");
        }
      });

    setPeaksLoading(true);
    ensurePeaks(id)
      .then((res) => {
        setPeaks(res.peaks);
        setPeaksLoading(false);
      })
      .catch((e) => {
        setError(errMsg(e));
        setPeaksLoading(false);
      });

    listBookmarks(id)
      .then(setBookmarks)
      .catch(() => undefined);

    listSegments(id)
      .then((segs) => setSegCount(segs.length))
      .catch(() => undefined);
  }, [id, fallbackRec]);

  // 键盘快捷键：空格播放/暂停，B 加书签（输入框聚焦时忽略）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) return;
      if (e.code === "Space") {
        e.preventDefault();
        const audio = audioRef.current;
        if (!audio) return;
        if (audio.paused) void audio.play();
        else audio.pause();
      } else if (e.key.toLowerCase() === "b") {
        e.preventDefault();
        void addBookmarkHere();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [addBookmarkHere]);

  /** 有未保存草稿时先确认；同意离开则清掉脏标记 */
  function confirmDiscardDirty(): boolean {
    if (tab !== "transcript" || !transcriptDirty) return true;
    if (window.confirm("转写稿有未保存的修改，离开将丢弃。确定离开吗？")) {
      setTranscriptDirty(false);
      return true;
    }
    return false;
  }

  function goBack() {
    if (confirmDiscardDirty()) go({ kind: from ?? "workbench" });
  }

  function switchTab(next: Tab) {
    if (next !== tab && !confirmDiscardDirty()) return;
    setTab(next);
  }

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
    const choice = window.confirm(`确定删除「${rec.title}」吗？此操作不可撤销。`);
    if (!choice) return;
    const wipe = window.confirm(
      "是否同时删除归档的音频文件？\n\n确定 = 彻底删除（连音频文件一起，不可恢复）\n取消 = 仅从库中移除（音频文件保留）",
    );
    try {
      await deleteRecording(id, wipe);
      go({ kind: "library" });
    } catch (e) {
      setError(errMsg(e));
    }
  }

  async function removeBookmark(bmId: number) {
    try {
      await deleteBookmark(bmId);
      setBookmarks((prev) => prev.filter((b) => b.id !== bmId));
    } catch (e) {
      setError(errMsg(e));
    }
  }

  async function doExport(kind: "md" | "srt" | "txt") {
    if (!rec) return;
    const extLabel = kind === "md" ? "Markdown 纪要" : kind === "srt" ? "字幕" : "纯文本";
    const safeTitle = rec.title.replace(/[\\/:*?"<>|]/g, "_").slice(0, 60);
    const dest = await saveDialog({
      title: `导出${extLabel}`,
      defaultPath: `${safeTitle}.${kind}`,
      filters: [{ name: extLabel, extensions: [kind] }],
    });
    if (!dest) return;
    try {
      await exportRecording(rec.id, kind, dest);
      setSavedFlash(true);
      setTimeout(() => setSavedFlash(false), 1500);
    } catch (e) {
      setError(errMsg(e));
    }
  }

  if (error && !rec) {
    return (
      <div className="h-full flex flex-col items-center justify-center gap-3">
        <div className="text-st-err text-sm">{error}</div>
        <button
          className="text-mist-400 hover:text-mist-200 text-sm"
          onClick={() => go({ kind: from ?? "workbench" })}
        >
          返回工作台
        </button>
      </div>
    );
  }

  if (!rec) {
    return (
      <div className="h-full flex items-center justify-center text-mist-400 text-sm">加载中…</div>
    );
  }

  const tabs: { key: Tab; label: string; hint?: string }[] = [
    { key: "transcript", label: "转写稿", hint: segCount > 0 ? String(segCount) : undefined },
    { key: "summary", label: "AI 纪要", hint: rec.summaryMd ? "✓" : undefined },
    { key: "info", label: "信息与元数据" },
  ];

  return (
    <div className="h-full flex flex-col relative">
      {/* 头部条 */}
      <div className="flex items-center gap-3 px-4 h-14 border-b border-ink-800 bg-ink-900/60 shrink-0">
        <button
          className="text-mist-400 hover:text-mist-200 text-sm shrink-0"
          onClick={goBack}
        >
          {from === "search" ? "← 返回搜索" : from === "library" ? "← 返回录音库" : "← 返回"}
        </button>
        <div className="min-w-0 flex items-baseline gap-2 flex-1">
          <span className="text-mist-100 truncate font-medium">{rec.title}</span>
          <span className="text-[11px] text-ink-500 shrink-0 tabular-nums">
            {STATUS_LABEL[rec.status]} · {formatDuration(rec.durationSec)} ·{" "}
            {formatDateTime(rec.recordedAt)}
          </span>
        </div>
        <details className="relative shrink-0">
          <summary className="list-none px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200 cursor-pointer select-none">
            导出 ▾
          </summary>
          <div className="absolute right-0 top-full mt-1 z-20 w-40 bg-ink-850 border border-ink-700 rounded-lg shadow-xl py-1 text-sm">
            <button
              className="w-full text-left px-3 py-1.5 text-mist-300 hover:bg-ink-700"
              onClick={() => void doExport("md")}
            >
              Markdown 纪要
            </button>
            <button
              className="w-full text-left px-3 py-1.5 text-mist-300 hover:bg-ink-700"
              onClick={() => void doExport("srt")}
            >
              SRT 字幕
            </button>
            <button
              className="w-full text-left px-3 py-1.5 text-mist-300 hover:bg-ink-700"
              onClick={() => void doExport("txt")}
            >
              纯文本
            </button>
          </div>
        </details>
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200 shrink-0"
          onClick={() => void revealItemInDir(rec.filePath).catch((e) => setError(errMsg(e)))}
        >
          打开位置
        </button>
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-st-err-soft hover:bg-st-err/20 text-st-err shrink-0"
          onClick={doDelete}
        >
          删除
        </button>
      </div>

      <div className="flex-1 min-h-0 flex">
        {/* 左栏：处理流水线 */}
        <PipelinePanel
          rec={rec}
          job={job}
          currentTime={currentTime}
          segmentCount={segCount}
          peaksReady={!peaksLoading && peaks.length > 0}
          onSeek={seek}
          onChanged={refreshRec}
        />

        {/* 右栏：工作区 */}
        <div className="flex-1 min-w-0 flex flex-col">
          {/* 播放器 */}
          <div className="px-4 pt-3 shrink-0">
            <div className="rounded-xl border border-ink-800 bg-ink-900 p-4">
              {peaksLoading ? (
                <div className="h-24 flex items-center justify-center text-mist-400 text-sm">
                  正在生成波形（首次打开需解码音频）…
                </div>
              ) : (
                <PlayerBar
                  filePath={rec.filePath}
                  peaks={peaks}
                  durationSec={rec.durationSec}
                  currentTime={currentTime}
                  playing={playing}
                  speed={speed}
                  audioRef={audioRef}
                  onTimeUpdate={handleTimeUpdate}
                  onPlayingChange={setPlaying}
                  onSeek={seek}
                  onSpeedChange={changeSpeed}
                  onAddBookmark={addBookmarkHere}
                  onLoadedMetadata={onAudioMetadata}
                  onLoadError={(m) => {
                    // 纯浏览器预览没有 asset 协议，这种「加载失败」不是真问题
                    if (isTauri()) setError(m);
                  }}
                />
              )}

              {bookmarks.length > 0 && (
                <div className="flex gap-1.5 flex-wrap mt-3 pt-3 border-t border-ink-800">
                  {bookmarks.map((bm) => (
                    <span
                      key={bm.id}
                      className="group inline-flex items-center gap-1.5 px-2 py-1 rounded-full bg-ink-800 hover:bg-ink-700 text-xs text-mist-300 cursor-pointer"
                      onClick={() => seek(bm.timeMs / 1000)}
                      title={bm.note || bm.label}
                    >
                      <span className="text-cyan-accent">⚑</span>
                      <span className="tabular-nums text-ink-500">{formatMsTime(bm.timeMs)}</span>
                      {bm.label}
                      <button
                        className="text-ink-500 hover:text-st-err opacity-0 group-hover:opacity-100"
                        onClick={(e) => {
                          e.stopPropagation();
                          void removeBookmark(bm.id);
                        }}
                      >
                        ✕
                      </button>
                    </span>
                  ))}
                </div>
              )}
            </div>
          </div>

          {/* 标签页 */}
          <div className="flex items-center gap-1 px-4 pt-3 shrink-0">
            {tabs.map((t) => (
              <button
                key={t.key}
                onClick={() => switchTab(t.key)}
                className={`px-3 py-1.5 rounded-lg text-sm transition-colors flex items-center gap-1.5 ${
                  tab === t.key
                    ? "bg-ink-800 text-mist-100 font-medium"
                    : "text-mist-400 hover:bg-ink-850 hover:text-mist-300"
                }`}
              >
                {t.label}
                {t.hint && (
                  <span
                    className={`text-[10px] tabular-nums ${
                      t.hint === "✓" ? "text-st-ok" : "text-ink-500"
                    }`}
                  >
                    {t.hint}
                  </span>
                )}
              </button>
            ))}
            <div className="flex-1" />
            <span className="text-[11px] text-ink-500">空格 播放/暂停 · B 加书签</span>
          </div>

          {/* 内容区 */}
          <div className="flex-1 min-h-0 overflow-y-auto p-4 pt-3">
            {tab === "transcript" && (
              <div className="h-full min-h-[400px] flex flex-col">
                <TranscriptPanel
                  rec={rec}
                  currentTime={currentTime}
                  seek={seek}
                  onDirtyChange={setTranscriptDirty}
                  onChanged={() => {
                    refreshRec();
                    listSegments(id)
                      .then((segs) => setSegCount(segs.length))
                      .catch(() => undefined);
                  }}
                />
              </div>
            )}

            {tab === "summary" && (
              <SummaryPanel rec={rec} seek={seek} onChanged={refreshRec} />
            )}

            {tab === "info" && (
              <div className="grid grid-cols-1 lg:grid-cols-2 gap-4 max-w-4xl">
                <div className="rounded-xl border border-ink-800 bg-ink-900 p-4 space-y-3">
                  <div className="text-mist-300 text-sm font-medium">信息</div>
                  <dl className="text-xs space-y-1.5">
                    <Row label="状态" value={STATUS_LABEL[rec.status]} />
                    <Row label="时长" value={formatDuration(rec.durationSec)} />
                    <Row label="大小" value={formatBytes(rec.sizeBytes)} />
                    <Row label="格式" value={rec.format.toUpperCase()} />
                    <Row label="录制时间" value={formatDateTime(rec.recordedAt)} />
                    <Row label="导入于" value={formatDateTime(rec.createdAt)} />
                    <Row label="原始文件" value={rec.origName} />
                    <Row label="转写分段" value={segCount ? `${segCount} 段` : "—"} />
                    <Row label="AI 纪要" value={rec.summaryMd ? `由 ${rec.llmModel ?? "LLM"} 生成` : "—"} />
                  </dl>
                </div>

                <div className="rounded-xl border border-ink-800 bg-ink-900 p-4 space-y-3">
                  <div className="flex items-center gap-2">
                    <div className="text-mist-300 text-sm font-medium">元数据</div>
                    <div className="flex-1" />
                    <button
                      className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200 disabled:opacity-50"
                      onClick={save}
                      disabled={saving}
                    >
                      {savedFlash ? "已保存 ✓" : saving ? "保存中…" : "保存"}
                    </button>
                  </div>
                  <label className="block space-y-1">
                    <span className="text-xs text-ink-500">标题</span>
                    <input
                      type="text"
                      className="w-full"
                      value={title}
                      onChange={(e) => {
                        titleDirtyRef.current = true;
                        setTitle(e.target.value);
                      }}
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
            )}
          </div>
        </div>
      </div>

      {error && (
        <div className="absolute bottom-4 left-1/2 -translate-x-1/2 px-4 py-2 rounded-lg bg-st-err-soft border border-st-err/40 text-st-err text-sm shadow-lg">
          {error}
          <button className="ml-3 text-st-err/70 hover:text-st-err" onClick={() => setError("")}>
            ✕
          </button>
        </div>
      )}
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-2">
      <dt className="text-ink-500 shrink-0">{label}</dt>
      <dd className="text-mist-300 truncate" title={value}>
        {value}
      </dd>
    </div>
  );
}
