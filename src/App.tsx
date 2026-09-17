import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import logoUrl from "./assets/logo.svg";
import { isTauri, type JobEvent } from "./api";
import { selectPendingCount, useAppStore } from "./store";
import WorkbenchView from "./views/WorkbenchView";
import LibraryView from "./views/LibraryView";
import SearchView from "./views/SearchView";
import DetailView from "./views/DetailView";
import SettingsView from "./views/SettingsView";
import type { View } from "./types";

function Icon({ kind }: { kind: string }) {
  const common = {
    width: 16,
    height: 16,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.8,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (kind) {
    case "workbench":
      return (
        <svg {...common}>
          <rect x="3" y="3" width="7" height="9" rx="1.5" />
          <rect x="14" y="3" width="7" height="5" rx="1.5" />
          <rect x="14" y="12" width="7" height="9" rx="1.5" />
          <rect x="3" y="16" width="7" height="5" rx="1.5" />
        </svg>
      );
    case "library":
      return (
        <svg {...common}>
          <path d="M3 8.5V7a2 2 0 0 1 2-2h3.2l1.6 2H19a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
        </svg>
      );
    case "search":
      return (
        <svg {...common}>
          <circle cx="11" cy="11" r="6.5" />
          <path d="m16 16 4 4" />
        </svg>
      );
    default:
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-2.9 1.2 2 2 0 1 1-4 0 1.7 1.7 0 0 0-2.9-1.2l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1A1.7 1.7 0 0 0 3 15a2 2 0 1 1 0-4 1.7 1.7 0 0 0 1.5-2.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1A1.7 1.7 0 0 0 10 4.6a2 2 0 1 1 4 0 1.7 1.7 0 0 0 2.9 1.2l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1A1.7 1.7 0 0 0 21 11a2 2 0 1 1 0 4z" />
        </svg>
      );
  }
}

function Sidebar() {
  const view = useAppStore((s) => s.view);
  const go = useAppStore((s) => s.go);
  const pending = useAppStore(selectPendingCount);
  const loaded = useAppStore((s) => s.recordingsLoaded);

  const items: { key: View["kind"]; label: string; icon: string; active: boolean }[] = [
    {
      key: "workbench",
      label: "工作台",
      icon: "workbench",
      active: view.kind === "workbench" || view.kind === "detail",
    },
    { key: "library", label: "录音库", icon: "library", active: view.kind === "library" },
    { key: "search", label: "全文搜索", icon: "search", active: view.kind === "search" },
    { key: "settings", label: "设置", icon: "settings", active: view.kind === "settings" },
  ];

  return (
    <aside className="w-56 shrink-0 flex flex-col bg-ink-900 border-r border-ink-800">
      <div className="flex items-center gap-2.5 px-4 h-14 border-b border-ink-800">
        <img src={logoUrl} alt="声迹" className="w-7 h-7 rounded-md" />
        <div className="leading-tight">
          <div className="text-sm font-semibold text-mist-200">声迹</div>
          <div className="text-[10px] text-ink-500 tracking-wider">SOUNDTRACE</div>
        </div>
      </div>

      <nav className="flex-1 p-2 space-y-0.5">
        {items.map((it) => (
          <button
            key={it.key}
            onClick={() => go({ kind: it.key } as View)}
            className={`w-full flex items-center gap-2.5 px-3 py-2 rounded-lg text-sm transition-colors ${
              it.active
                ? "bg-ink-800 text-mist-100"
                : "text-mist-400 hover:bg-ink-850 hover:text-mist-300"
            }`}
          >
            <span className={it.active ? "text-cyan-accent" : "text-ink-500"}>
              <Icon kind={it.icon} />
            </span>
            <span className="flex-1 text-left">{it.label}</span>
            {it.key === "workbench" && loaded && pending > 0 && (
              <span
                className="px-1.5 py-0.5 rounded-full bg-st-info-soft text-st-info text-[10px] tabular-nums font-medium"
                title={`${pending} 条待处理`}
              >
                {pending}
              </span>
            )}
          </button>
        ))}
      </nav>

      <div className="p-3 text-[11px] text-ink-500 border-t border-ink-800 leading-relaxed">
        <div className="flex items-center gap-1.5">
          <span className="w-1.5 h-1.5 rounded-full bg-jade-accent" />
          本地运行 · 数据不出本机
        </div>
        <div className="mt-0.5 opacity-80">转写在本机完成，仅 AI 纪要需联网</div>
      </div>
    </aside>
  );
}

export default function App() {
  const view = useAppStore((s) => s.view);
  const refreshAll = useAppStore((s) => s.refreshAll);
  const refreshJobs = useAppStore((s) => s.refreshJobs);
  const initTheme = useAppStore((s) => s.initTheme);

  useEffect(() => {
    initTheme();
    void refreshAll();
  }, [initTheme, refreshAll]);

  // 任务状态变化时刷新库列表 + 队列（列表页与工作台都能看到状态流转）。
  // running 的进度事件每秒可达两次，用定时器合并，避免批量入队时抖动 N 次全量查询。
  useEffect(() => {
    if (!isTauri()) return;
    let timer: number | undefined;
    const un = listen<JobEvent>("job://progress", (e) => {
      if (e.payload.status === "running" && e.payload.progress > 0) return;
      if (timer !== undefined) return;
      timer = window.setTimeout(() => {
        timer = undefined;
        void useAppStore.getState().refreshRecordings();
        void refreshJobs();
      }, 200);
    });
    return () => {
      if (timer !== undefined) window.clearTimeout(timer);
      void un.then((fn) => fn());
    };
  }, [refreshJobs]);

  return (
    <div className="h-full flex">
      <Sidebar />
      <main className="flex-1 min-w-0 overflow-hidden">
        {view.kind === "workbench" && <WorkbenchView />}
        {view.kind === "library" && <LibraryView />}
        {view.kind === "search" && <SearchView />}
        {view.kind === "detail" && <DetailView key={view.id} id={view.id} seekMs={view.seekMs} />}
        {view.kind === "settings" && <SettingsView />}
      </main>
    </div>
  );
}
