import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import logoUrl from "./assets/logo.svg";
import { isTauri, type JobEvent } from "./api";
import { useAppStore } from "./store";
import LibraryView from "./views/LibraryView";
import DetailView from "./views/DetailView";
import SettingsView from "./views/SettingsView";

function Sidebar() {
  const view = useAppStore((s) => s.view);
  const go = useAppStore((s) => s.go);

  const items = [
    { key: "library", label: "录音库", active: view.kind === "library" || view.kind === "detail" },
    { key: "settings", label: "设置", active: view.kind === "settings" },
  ] as const;

  return (
    <aside className="w-56 shrink-0 flex flex-col bg-ink-900 border-r border-ink-800">
      <div className="flex items-center gap-2.5 px-4 h-14 border-b border-ink-800">
        <img src={logoUrl} alt="声迹" className="w-7 h-7 rounded-md" />
        <div className="leading-tight">
          <div className="text-sm font-semibold text-mist-200">声迹</div>
          <div className="text-[10px] text-ink-500 tracking-wider">SOUNDTRACE</div>
        </div>
      </div>

      <nav className="flex-1 p-2 space-y-1">
        {items.map((it) => (
          <button
            key={it.key}
            onClick={() => go(it.key === "library" ? { kind: "library" } : { kind: "settings" })}
            className={`w-full text-left px-3 py-2 rounded-lg text-sm transition-colors ${
              it.active
                ? "bg-ink-800 text-mist-200"
                : "text-mist-400 hover:bg-ink-850 hover:text-mist-300"
            }`}
          >
            {it.label}
          </button>
        ))}
      </nav>

      <div className="p-3 text-[11px] text-ink-500 border-t border-ink-800">
        <div>本地录音库 · 数据不出本机</div>
      </div>
    </aside>
  );
}

export default function App() {
  const view = useAppStore((s) => s.view);
  const refresh = useAppStore((s) => s.refreshRecordings);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 转写任务状态变化时刷新库列表（列表页也能看到状态流转）
  useEffect(() => {
    if (!isTauri()) return;
    const un = listen<JobEvent>("job://progress", (e) => {
      if (e.payload.status !== "running") {
        void useAppStore.getState().refreshRecordings();
      }
    });
    return () => {
      void un.then((fn) => fn());
    };
  }, []);

  return (
    <div className="h-full flex">
      <Sidebar />
      <main className="flex-1 min-w-0 overflow-hidden">
        {view.kind === "library" && <LibraryView />}
        {view.kind === "detail" && <DetailView key={view.id} id={view.id} seekMs={view.seekMs} />}
        {view.kind === "settings" && <SettingsView />}
      </main>
    </div>
  );
}
