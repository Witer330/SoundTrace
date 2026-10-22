import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isTauri } from "../api";

/**
 * 拖拽音频进窗口：drop 时回调路径（文件或目录，由 importPaths 展开导入），
 * 返回是否拖拽中（用于挂遮罩）。回调经 ref 转发，避免 effect 闭包过期。
 */
export function useAudioDrop(onDrop: (paths: string[]) => void): boolean {
  const [dragging, setDragging] = useState(false);
  const depth = useRef(0);
  const cbRef = useRef(onDrop);
  cbRef.current = onDrop;

  useEffect(() => {
    if (!isTauri()) return;
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "enter") {
        depth.current += 1;
        setDragging(true);
      } else if (p.type === "leave") {
        depth.current = Math.max(0, depth.current - 1);
        if (depth.current === 0) setDragging(false);
      } else if (p.type === "drop") {
        depth.current = 0;
        setDragging(false);
        cbRef.current(p.paths);
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  return dragging;
}
