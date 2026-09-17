import { STATUS_LABEL, type RecordingStatus } from "../types";

const STYLES: Record<RecordingStatus, string> = {
  imported: "bg-st-muted-soft text-st-muted",
  queued: "bg-st-warn-soft text-st-warn",
  transcribing: "bg-st-info-soft text-st-info animate-pulse",
  done: "bg-st-ok-soft text-st-ok",
  failed: "bg-st-err-soft text-st-err",
};

/** 录音处理状态徽章（已导入 / 排队中 / 转写中 / 已转写 / 转写失败） */
export default function StatusBadge({ status }: { status: RecordingStatus }) {
  return (
    <span className={`px-2 py-0.5 rounded text-xs shrink-0 ${STYLES[status]}`}>
      {STATUS_LABEL[status]}
    </span>
  );
}
