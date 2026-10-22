import { useCallback, useEffect, useRef, useState } from "react";
import { useAppStore } from "../store";
import { formatMsTime } from "../types";

interface WaveformProps {
  peaks: number[];
  durationSec: number;
  currentTime: number;
  onSeek: (sec: number) => void;
  height?: number;
}

/** 变量名与 styles.css 的令牌对应；Canvas 取不到 CSS 继承色，只能运行时读变量 */
const COLOR_VARS = {
  idle: "--color-ink-700",
  played: "--color-cyan-accent",
  unplayed: "--color-ink-600",
  playhead: "--color-teal-accent",
} as const;

function readColors(el: Element): Record<keyof typeof COLOR_VARS, string> {
  const cs = getComputedStyle(el);
  const out = {} as Record<keyof typeof COLOR_VARS, string>;
  for (const [key, name] of Object.entries(COLOR_VARS)) {
    out[key as keyof typeof COLOR_VARS] = cs.getPropertyValue(name).trim();
  }
  return out;
}

/**
 * 预计算峰值波形（Canvas）。
 * 已播放部分青色、未播放暗蓝；点击/拖动跳转。所有交互换算基于 durationSec。
 */
export default function Waveform({
  peaks,
  durationSec,
  currentTime,
  onSeek,
  height = 96,
}: WaveformProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const draggingRef = useRef(false);
  // 悬停/拖动时显示目标时间戳
  const [hover, setHover] = useState<{ x: number; sec: number } | null>(null);
  // 主题变化时重绘（暂停状态下 currentTime 不变，否则波形会停在旧配色）
  const theme = useAppStore((s) => s.theme);

  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const parent = canvas.parentElement;
    if (!parent) return;
    const dpr = window.devicePixelRatio || 1;
    const cssW = parent.clientWidth;
    const cssH = height;
    if (canvas.width !== Math.floor(cssW * dpr) || canvas.height !== Math.floor(cssH * dpr)) {
      canvas.width = Math.floor(cssW * dpr);
      canvas.height = Math.floor(cssH * dpr);
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);

    const mid = cssH / 2;
    const progress = durationSec > 0 ? Math.min(currentTime / durationSec, 1) : 0;
    const progressX = progress * cssW;
    const colors = readColors(canvas);

    if (peaks.length === 0) {
      ctx.fillStyle = colors.idle;
      ctx.fillRect(0, mid - 1, cssW, 2);
      return;
    }

    for (let x = 0; x < cssW; x++) {
      // 每像素列对应的峰值区间
      const start = Math.floor((x / cssW) * peaks.length);
      const end = Math.max(start + 1, Math.floor(((x + 1) / cssW) * peaks.length));
      let peak = 0;
      for (let i = start; i < end && i < peaks.length; i++) {
        if (peaks[i] > peak) peak = peaks[i];
      }
      // 平方根缩放，让小音量也可见
      const amp = Math.max(1.5, Math.sqrt(peak) * (cssH / 2 - 4));
      ctx.fillStyle = x <= progressX ? colors.played : colors.unplayed;
      ctx.fillRect(x, mid - amp, 1, amp * 2);
    }

    // 播放头
    ctx.fillStyle = colors.playhead;
    ctx.fillRect(progressX - 0.5, 0, 1.5, cssH);
  }, [peaks, durationSec, currentTime, height, theme]);

  useEffect(() => {
    draw();
    const onResize = () => draw();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [draw]);

  const posFromClientX = useCallback(
    (clientX: number): { x: number; sec: number } | null => {
      const canvas = canvasRef.current;
      if (!canvas || durationSec <= 0) return null;
      const rect = canvas.getBoundingClientRect();
      const frac = Math.min(Math.max((clientX - rect.left) / rect.width, 0), 1);
      return { x: frac * rect.width, sec: frac * durationSec };
    },
    [durationSec],
  );

  const seekFromEvent = useCallback(
    (clientX: number) => {
      const canvas = canvasRef.current;
      if (!canvas || durationSec <= 0) return;
      const rect = canvas.getBoundingClientRect();
      const frac = Math.min(Math.max((clientX - rect.left) / rect.width, 0), 1);
      onSeek(frac * durationSec);
    },
    [durationSec, onSeek],
  );

  return (
    <div className="relative w-full select-none" style={{ height }}>
      <canvas
        ref={canvasRef}
        className="w-full h-full cursor-pointer"
        onPointerDown={(e) => {
          draggingRef.current = true;
          e.currentTarget.setPointerCapture(e.pointerId);
          seekFromEvent(e.clientX);
        }}
        onPointerMove={(e) => {
          const p = posFromClientX(e.clientX);
          if (p) setHover(p);
          if (draggingRef.current) seekFromEvent(e.clientX);
        }}
        onPointerLeave={() => setHover(null)}
        onPointerUp={(e) => {
          draggingRef.current = false;
          e.currentTarget.releasePointerCapture(e.pointerId);
        }}
      />
      {hover && (
        <div
          className="pointer-events-none absolute -top-6 -translate-x-1/2 rounded border border-ink-700 bg-ink-950/90 px-1.5 py-0.5 text-[10px] text-mist-300 tabular-nums whitespace-nowrap"
          style={{ left: hover.x }}
        >
          {formatMsTime(hover.sec * 1000)}
        </div>
      )}
    </div>
  );
}
