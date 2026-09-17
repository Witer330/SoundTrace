import { useCallback, useEffect, useRef } from "react";

interface WaveformProps {
  peaks: number[];
  durationSec: number;
  currentTime: number;
  onSeek: (sec: number) => void;
  height?: number;
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

    if (peaks.length === 0) {
      ctx.fillStyle = "#24365c";
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
      ctx.fillStyle = x <= progressX ? "#22d3ee" : "#324673";
      ctx.fillRect(x, mid - amp, 1, amp * 2);
    }

    // 播放头
    ctx.fillStyle = "#5eead4";
    ctx.fillRect(progressX - 0.5, 0, 1.5, cssH);
  }, [peaks, durationSec, currentTime, height]);

  useEffect(() => {
    draw();
    const onResize = () => draw();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [draw]);

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
          if (draggingRef.current) seekFromEvent(e.clientX);
        }}
        onPointerUp={(e) => {
          draggingRef.current = false;
          e.currentTarget.releasePointerCapture(e.pointerId);
        }}
      />
    </div>
  );
}
