import { Component, type ReactNode } from "react";

import type { CustomViewProps } from "./registry";

interface BoundaryProps {
  children: ReactNode;
  onError: (message: string) => void;
  resetKey: string;
}

interface BoundaryState {
  failed: boolean;
}

/// 自定义 View 加载失败（lazy chunk 错误等）时回退通用渲染并上抛提示。
export class CustomViewBoundary extends Component<
  BoundaryProps,
  BoundaryState
> {
  state: BoundaryState = { failed: false };

  static getDerivedStateFromError(): BoundaryState {
    return { failed: true };
  }

  componentDidCatch(error: unknown) {
    this.props.onError(error instanceof Error ? error.message : String(error));
  }

  componentDidUpdate(prev: BoundaryProps) {
    if (prev.resetKey !== this.props.resetKey && this.state.failed) {
      this.setState({ failed: false });
    }
  }

  render() {
    if (this.state.failed) return null;
    return this.props.children;
  }
}

export type { CustomViewProps };
