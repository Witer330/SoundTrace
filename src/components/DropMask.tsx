/** 拖拽导入的全屏遮罩（useAudioDrop 返回 true 时挂上） */
export default function DropMask() {
  return (
    <div className="absolute inset-0 z-10 flex items-center justify-center bg-ink-950/60 backdrop-blur-sm">
      <div className="border-2 border-dashed border-cyan-accent/50 rounded-2xl px-16 py-12 text-center">
        <div className="text-cyan-accent text-lg">松手导入录音</div>
        <div className="text-mist-400 text-sm mt-1">支持文件或文件夹（递归扫描）</div>
      </div>
    </div>
  );
}
