import { create } from "zustand";
import type { Recording, View } from "./types";
import { isTauri, listJobs, listRecordings, type JobRow } from "./api";

export type ThemeMode = "system" | "light" | "dark";
export type Theme = "light" | "dark";

const THEME_KEY = "st.theme";

function systemTheme(): Theme {
  return window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/** 把已解析的主题写到 <html data-theme>，驱动 styles.css 里的变量切换 */
function applyTheme(theme: Theme) {
  const root = document.documentElement;
  root.dataset.theme = theme;
  // 同步 color-scheme，让滚动条与原生控件（含 <select> 下拉）跟随主题
  root.style.colorScheme = theme;
}

function readStoredMode(): ThemeMode {
  const v = localStorage.getItem(THEME_KEY);
  return v === "light" || v === "dark" || v === "system" ? v : "system";
}

interface AppState {
  view: View;
  recordings: Recording[];
  recordingsLoaded: boolean;
  jobs: JobRow[];
  libraryQuery: string;
  theme: Theme;
  themeMode: ThemeMode;
  go: (view: View) => void;
  setLibraryQuery: (q: string) => void;
  refreshRecordings: () => Promise<void>;
  refreshJobs: () => Promise<void>;
  /** 录音列表 + 队列一起刷新（工作台与库列表共用） */
  refreshAll: () => Promise<void>;
  initTheme: () => void;
  setThemeMode: (mode: ThemeMode) => void;
}

export const useAppStore = create<AppState>((set, get) => ({
  view: { kind: "workbench" },
  recordings: [],
  recordingsLoaded: false,
  jobs: [],
  libraryQuery: "",
  theme: "dark",
  themeMode: "system",

  go: (view) => {
    set({ view });
    // 进入列表类视图时刷新（详情页可能改过元数据、队列可能已推进）
    if (view.kind === "library" || view.kind === "workbench" || view.kind === "search") {
      set((s) => {
        if (s.recordingsLoaded) void s.refreshAll();
        return s;
      });
    }
  },

  setLibraryQuery: (q) => set({ libraryQuery: q }),

  refreshRecordings: async () => {
    if (!isTauri()) {
      // 纯浏览器调试环境：无后端，显示空库
      set({ recordings: [], recordingsLoaded: true });
      return;
    }
    try {
      const recordings = await listRecordings();
      set({ recordings, recordingsLoaded: true });
    } catch {
      // 后端未就绪时保持现状，由调用方决定是否提示
    }
  },

  refreshJobs: async () => {
    if (!isTauri()) return;
    try {
      set({ jobs: await listJobs(false) });
    } catch {
      // 忽略：队列面板允许为空
    }
  },

  refreshAll: async () => {
    await Promise.all([get().refreshRecordings(), get().refreshJobs()]);
  },

  initTheme: () => {
    const mode = readStoredMode();
    const theme = mode === "system" ? systemTheme() : mode;
    applyTheme(theme);
    set({ themeMode: mode, theme });
    // 「跟随系统」时响应系统切换；手动模式下 media 变化不影响
    window.matchMedia?.("(prefers-color-scheme: light)").addEventListener("change", () => {
      if (get().themeMode !== "system") return;
      const next = systemTheme();
      applyTheme(next);
      set({ theme: next });
    });
  },

  setThemeMode: (mode) => {
    localStorage.setItem(THEME_KEY, mode);
    const theme = mode === "system" ? systemTheme() : mode;
    applyTheme(theme);
    set({ themeMode: mode, theme });
  },
}));

/** 队列中的活跃任务（worker 串行，至多一个 running） */
export function selectActiveJob(s: AppState): JobRow | undefined {
  return s.jobs.find((j) => j.status === "running") ?? s.jobs.find((j) => j.status === "pending");
}

/** 待处理数：排队中 + 转写中 */
export function selectPendingCount(s: AppState): number {
  return s.jobs.filter((j) => j.status === "pending" || j.status === "running").length;
}

// 调试与端到端验证用：在浏览器控制台里可直接切换视图/主题
if (import.meta.env.DEV) {
  (window as unknown as { __store: typeof useAppStore }).__store = useAppStore;
}
