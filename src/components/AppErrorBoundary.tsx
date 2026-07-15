import { Component, type ErrorInfo, type ReactNode } from "react";
import "./AppErrorBoundary.css";

type AppErrorBoundaryProps = {
  children: ReactNode;
  onReload?: () => void;
};

type AppErrorBoundaryState = {
  failed: boolean;
};

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): AppErrorBoundaryState {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[GXPlayer] frontend render failed", error, info);
  }

  private reload = () => {
    if (this.props.onReload) {
      this.props.onReload();
      return;
    }
    window.location.reload();
  };

  render() {
    if (!this.state.failed) return this.props.children;

    return (
      <main className="app-error-boundary" role="alert" aria-labelledby="app-error-title">
        <section className="app-error-card">
          <p className="app-error-kicker">GXPLAYER</p>
          <h1 id="app-error-title">界面暂时无法显示</h1>
          <p>播放器遇到了未处理的前端错误。重新载入通常可以恢复，播放文件不会因此被删除。</p>
          <button type="button" onClick={this.reload}>重新载入</button>
        </section>
      </main>
    );
  }
}
