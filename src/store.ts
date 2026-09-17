import { create } from "zustand";
import type { Recording, View } from "./types";
import { isTauri, listRecordings } from "./api";

interface AppState {
  view: View;
  recordings: Recording[];
  recordingsLoaded: boolean;
  libraryQuery: string;
  go: (view: View) => void;
  setLibraryQuery: (q: string) => void;
  refreshRecordings: () => Promise<void>;
}

export const useAppStore = create<AppState>((set) => ({
  view: { kind: "library" },
  recordings: [],
  recordingsLoaded: false,
  libraryQuery: "",

  go: (view) => {
    set({ view });
    if (view.kind === "library") {
      // 从详情返回时刷新列表（元数据可能被修改过）
      set((s) => {
        if (s.recordingsLoaded) void s.refreshRecordings();
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
}));
