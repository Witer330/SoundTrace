import { useEffect, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { listen } from "@tauri-apps/api/event";
import { errMsg, generateSummary } from "../api";
import type { Recording } from "../types";

interface Props {
  rec: Recording;
  onChanged: () => void;
}

/** AI 复盘面板：生成/重新生成会议纪要（Markdown 渲染） */
export default function SummaryPanel({ rec, onChanged }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    const un = listen<{ recordingId: number; ok: boolean; error: string }>(
      "summarize://done",
      (e) => {
        if (e.payload.recordingId !== rec.id) return;
        setBusy(false);
        if (e.payload.ok) {
          onChanged();
        } else {
          setError(e.payload.error || "生成失败");
        }
      },
    );
    return () => {
      void un.then((fn) => fn());
    };
  }, [rec.id, onChanged]);

  function start() {
    setError("");
    setBusy(true);
    generateSummary(rec.id).catch((e) => {
      setBusy(false);
      setError(errMsg(e));
    });
  }

  const hasTranscript = rec.status === "done";

  return (
    <div className="rounded-xl border border-ink-800 bg-ink-900 p-4 space-y-3">
      <div className="flex items-center gap-2">
        <div className="text-mist-300 text-sm font-medium">AI 复盘 · 会议纪要</div>
        <div className="flex-1" />
        {hasTranscript && (
          <button
            className="px-3 py-1.5 rounded-lg text-sm bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium hover:opacity-90 disabled:opacity-50"
            onClick={start}
            disabled={busy}
          >
            {busy
              ? "生成中…"
              : rec.summaryMd
                ? "重新生成"
                : "生成纪要"}
          </button>
        )}
      </div>

      {!hasTranscript && (
        <div className="text-mist-400 text-xs">完成转写后可在此生成 AI 会议纪要。</div>
      )}
      {hasTranscript && !rec.summaryMd && !busy && (
        <div className="text-mist-400 text-xs">
          将转写稿发送给所配置的 LLM，生成概要 / 关键决议 / 行动项 / 待澄清问题。
        </div>
      )}
      {busy && (
        <div className="text-mist-400 text-xs animate-pulse">
          正在生成（长会议需 1~3 分钟）… · 仅转写文本出网，录音不出本机
        </div>
      )}
      {error && <div className="text-xs text-red-300">{error}</div>}

      {rec.summaryMd && (
        <div className="prose-invert max-w-none text-sm leading-6 text-mist-200 [&_h2]:text-mist-100 [&_h2]:mt-4 [&_h2]:mb-2 [&_h2]:text-base [&_h2]:font-semibold [&_h3]:text-mist-200 [&_table]:w-full [&_table]:text-xs [&_th]:border [&_th]:border-ink-700 [&_th]:px-2 [&_th]:py-1 [&_th]:bg-ink-850 [&_td]:border [&_td]:border-ink-700 [&_td]:px-2 [&_td]:py-1 [&_ul]:list-disc [&_ul]:pl-5 [&_ol]:list-decimal [&_ol]:pl-5 [&_li]:my-0.5 [&_hr]:border-ink-700 [&_strong]:text-mist-100">
          <Markdown remarkPlugins={[remarkGfm]}>{rec.summaryMd}</Markdown>
        </div>
      )}
      {rec.summaryMd && rec.llmModel && (
        <div className="text-[11px] text-ink-500">由 {rec.llmModel} 生成</div>
      )}
    </div>
  );
}
