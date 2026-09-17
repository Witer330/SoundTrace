import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect } from "react";
import Waveform from "./Waveform";
import { formatMsTime } from "../types";

const SPEEDS = [0.5, 0.75, 1, 1.25, 1.5, 2, 3];

interface PlayerBarProps {
  filePath: string;
  peaks: number[];
  durationSec: number;
  currentTime: number;
  playing: boolean;
  speed: number;
  audioRef: React.RefObject<HTMLAudioElement | null>;
  onTimeUpdate: (sec: number) => void;
  onPlayingChange: (playing: boolean) => void;
  onSeek: (sec: number) => void;
  onSpeedChange: (speed: number) => void;
  onAddBookmark: () => void;
}

/** 音频播放器：波形 + 控制（播放/快进快退/倍速/书签） */
export default function PlayerBar({
  filePath,
  peaks,
  durationSec,
  currentTime,
  playing,
  speed,
  audioRef,
  onTimeUpdate,
  onPlayingChange,
  onSeek,
  onSpeedChange,
  onAddBookmark,
}: PlayerBarProps) {
  const src = convertFileSrc(filePath);

  useEffect(() => {
    const audio = audioRef.current;
    if (audio) audio.playbackRate = speed;
  }, [speed, audioRef]);

  function toggle() {
    const audio = audioRef.current;
    if (!audio) return;
    if (audio.paused) {
      void audio.play();
    } else {
      audio.pause();
    }
  }

  function skip(deltaSec: number) {
    const audio = audioRef.current;
    if (!audio) return;
    const target = Math.min(Math.max(audio.currentTime + deltaSec, 0), durationSec || audio.duration || 0);
    onSeek(target);
  }

  return (
    <div className="space-y-3">
      <audio
        ref={audioRef}
        src={src}
        preload="metadata"
        onTimeUpdate={(e) => onTimeUpdate(e.currentTarget.currentTime)}
        onPlay={() => onPlayingChange(true)}
        onPause={() => onPlayingChange(false)}
        onEnded={() => onPlayingChange(false)}
      />

      <Waveform
        peaks={peaks}
        durationSec={durationSec}
        currentTime={currentTime}
        onSeek={onSeek}
      />

      <div className="flex items-center gap-2">
        <button
          className="w-9 h-9 rounded-full bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 flex items-center justify-center hover:opacity-90 transition-opacity"
          onClick={toggle}
          title={playing ? "暂停（空格）" : "播放（空格）"}
        >
          {playing ? (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
              <rect x="5" y="4" width="5" height="16" rx="1" />
              <rect x="14" y="4" width="5" height="16" rx="1" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
              <path d="M7 4.5v15l13-7.5-13-7.5z" />
            </svg>
          )}
        </button>

        <button
          className="px-2.5 h-8 rounded-lg bg-ink-700 hover:bg-ink-600 text-mist-300 text-xs"
          onClick={() => skip(-10)}
          title="后退 10 秒"
        >
          ↺10s
        </button>
        <button
          className="px-2.5 h-8 rounded-lg bg-ink-700 hover:bg-ink-600 text-mist-300 text-xs"
          onClick={() => skip(30)}
          title="前进 30 秒"
        >
          30s↻
        </button>

        <div className="text-xs text-mist-400 tabular-nums">
          {formatMsTime(currentTime * 1000)} / {formatMsTime(durationSec * 1000)}
        </div>

        <div className="flex-1" />

        <select
          className="h-8 text-xs bg-ink-850 border border-ink-700 rounded-lg px-2 text-mist-300"
          value={speed}
          onChange={(e) => onSpeedChange(Number(e.target.value))}
          title="播放倍速"
        >
          {SPEEDS.map((s) => (
            <option key={s} value={s}>
              {s}x
            </option>
          ))}
        </select>

        <button
          className="px-2.5 h-8 rounded-lg bg-ink-700 hover:bg-ink-600 text-mist-300 text-xs"
          onClick={onAddBookmark}
          title="在当前位置添加书签（B）"
        >
          ⚑ 书签
        </button>
      </div>
    </div>
  );
}
