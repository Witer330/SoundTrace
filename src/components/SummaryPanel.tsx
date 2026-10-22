import { useEffect, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { listen } from "@tauri-apps/api/event";
import { errMsg, generateSummary } from "../api";
import { useAppStore } from "../store";
import type { Recording } from "../types";

interface Props {
  rec: Recording;
  seek: (sec: number) => void;
  onChanged: () => void;
}

/** 把纪要里的 [HH:MM:SS] / [MM:SS] 时间戳换成页内链接（#st-秒数），渲染层再转成跳转按钮 */
function linkifyTimestamps(md: string): string {
  return md.replace(/\[(\d{1,2}:\d{2}(?::\d{2})?)\](?!\()/g, (_all, t: string) => {
    const p = t.split(":").map(Number);
    const sec =
      p.length === 3 ? p[0] * 3600 + p[1] * 60 + p[2] : p[0] * 60 + p[1];
    return `[${t}](#st-${sec})`;
  });
}

interface Section {
  title: string;
  body: string;
}

/** 按 `## 标题` 把纪要切成 (开头引言 + 分区)；旧格式（三段式）同样适用 */
function splitSections(md: string): { intro: string; sections: Section[] } {
  const introLines: string[] = [];
  const sections: Section[] = [];
  let cur: Section | null = null;
  for (const line of md.split("\n")) {
    const m = /^##\s+(.+)$/.exec(line);
    if (m) {
      cur = { title: m[1].trim(), body: "" };
      sections.push(cur);
    } else if (cur) {
      cur.body += line + "\n";
    } else {
      introLines.push(line);
    }
  }
  return { intro: introLines.join("\n").trim(), sections };
}

/** 分区标题栏的条数徽标：列表项数；标签分区数反引号词 */
function countItems(title: string, body: string): number {
  const tags = body.match(/`[^`]+`/g);
  if (title.includes("标签") && tags) return tags.length;
  return (body.match(/^\s*[-*]\s+/gm) || []).length;
}

/** AI 复盘面板：分区卡片（默认收起、点开按需展开），时间戳可点击跳回原声 */
export default function SummaryPanel({ rec, seek, onChanged }: Props) {
  // 生成态记在全局 store（后台线程无 job 行），从流水线或本页触发都能看到
  const busy = useAppStore((s) => s.summarizingId === rec.id);
  const setSummarizingId = useAppStore((s) => s.setSummarizingId);
  const [error, setError] = useState("");
  const [open, setOpen] = useState<Record<string, boolean>>({});

  useEffect(() => {
    const un = listen<{ recordingId: number; ok: boolean; error: string }>(
      "summarize://done",
      (e) => {
        if (e.payload.recordingId !== rec.id) return;
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
    if (busy) return;
    setError("");
    setSummarizingId(rec.id);
    generateSummary(rec.id).catch((e) => {
      setSummarizingId(null);
      setError(errMsg(e));
    });
  }

  const hasTranscript = rec.status === "done";
  const { intro, sections } = splitSections(rec.summaryMd ?? "");
  const toggle = (title: string) =>
    setOpen((o) => ({ ...o, [title]: !o[title] }));

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
          转写稿交给 LLM 提炼：关键结论 / 行动项 / 悬而未决 / 名词速查；时间戳可点击跳回原声。
        </div>
      )}
      {busy && (
        <div className="text-mist-400 text-xs animate-pulse">
          正在生成… · 仅转写文本出网，录音不出本机
        </div>
      )}
      {error && <div className="text-xs text-st-err">{error}</div>}

      {rec.summaryMd && intro && (
        <div className="md-body text-sm leading-6 text-mist-200 border-l-2 border-cyan-accent/50 pl-3">
          <Markdown>{linkifyTimestamps(intro)}</Markdown>
        </div>
      )}

      {rec.summaryMd &&
        sections.map((s) => {
          const n = countItems(s.title, s.body);
          const isOpen = !!open[s.title];
          return (
            <div
              key={s.title}
              className="rounded-lg border border-ink-800 bg-ink-850/60"
            >
              <button
                className="w-full flex items-center gap-2 px-3 py-2 text-left"
                onClick={() => toggle(s.title)}
              >
                <span className="text-ink-500 text-xs w-3">{isOpen ? "▾" : "▸"}</span>
                <span className="text-sm font-medium text-mist-200">{s.title}</span>
                <span className="flex-1" />
                <span className="text-[11px] text-ink-500 tabular-nums">
                  {n > 0 ? `${n} 条` : "无"}
                </span>
              </button>
              {isOpen && (
                <div className="md-body px-3 pb-3 text-sm leading-6 text-mist-200">
                  <Markdown
                    remarkPlugins={[remarkGfm]}
                    components={{
                      // 时间戳页内链接 → 跳转按钮；其余链接照常渲染
                      a: ({ href, children }) => {
                        const m = /^#st-(\d+)$/.exec(href ?? "");
                        if (!m) return <a href={href}>{children}</a>;
                        return (
                          <button
                            className="text-cyan-accent hover:underline cursor-pointer tabular-nums"
                            title="跳转到对应音频位置"
                            onClick={() => seek(Number(m[1]))}
                          >
                            {children}
                          </button>
                        );
                      },
                    }}
                  >
                    {linkifyTimestamps(s.body)}
                  </Markdown>
                </div>
              )}
            </div>
          );
        })}

      {rec.summaryMd && rec.llmModel && (
        <div className="text-[11px] text-ink-500">由 {rec.llmModel} 生成</div>
      )}
    </div>
  );
}
