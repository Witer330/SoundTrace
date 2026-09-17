import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
  info: string;
}

/**
 * 顶层错误边界：任何视图渲染抛错都不该让整个应用变成白屏。
 * 报错时展示可读信息与「重试」，并把细节留在 DOM 上便于排查。
 */
export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, info: "" };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // 控制台留痕（本地工具，不做上报）
    console.error("[声迹] 界面渲染失败：", error, info.componentStack);
    this.setState({ info: info.componentStack ?? "" });
  }

  render() {
    const { error, info } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="h-full flex items-center justify-center p-8">
        <div className="max-w-xl w-full rounded-xl border border-st-err/40 bg-st-err-soft p-5 space-y-3">
          <div className="text-st-err font-medium text-sm">界面渲染出错</div>
          <div className="text-xs text-st-err/90 font-mono break-all">{error.message}</div>
          <details className="text-[11px] text-mist-400">
            <summary className="cursor-pointer hover:text-mist-200">组件调用栈</summary>
            <pre className="mt-2 whitespace-pre-wrap break-all max-h-56 overflow-y-auto">
              {info}
            </pre>
          </details>
          <div className="flex gap-2">
            <button
              className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200"
              onClick={() => this.setState({ error: null, info: "" })}
            >
              重试
            </button>
            <button
              className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200"
              onClick={() => window.location.reload()}
            >
              重新加载
            </button>
          </div>
        </div>
      </div>
    );
  }
}
