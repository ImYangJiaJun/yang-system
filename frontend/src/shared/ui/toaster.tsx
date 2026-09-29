import { useEffect } from "react";
import {
  CheckCircle2,
  AlertCircle,
  Info,
  AlertTriangle,
  X,
} from "lucide-react";

import {
  useToastContext,
  type Toast,
  type ToastType,
} from "@/shared/providers/toast-provider";
import { Button } from "@/shared/ui/button";
import { cn } from "@/shared/lib/utils";

/**
 * Toast 通知渲染组件。
 *
 * 设计约束：
 * 1. **固定定位**：右上角，不占布局空间。
 * 2. **自动关闭**：每条 toast 按 duration 自动消失。
 * 3. **手动关闭**：点击关闭按钮立即移除。
 * 4. **操作按钮**：可选，用于「重发」「重试」等快捷操作。
 * 5. **动画**：滑入 + 淡出，不突兀。
 */

const ICONS: Record<ToastType, React.ComponentType<{ className?: string }>> = {
  success: CheckCircle2,
  error: AlertCircle,
  info: Info,
  warning: AlertTriangle,
};

const STYLES: Record<ToastType, string> = {
  success: "border-tone-positive/40 bg-tone-positive/10 text-tone-positive",
  error: "border-destructive/40 bg-destructive/10 text-destructive",
  info: "border-tone-info/40 bg-tone-info/10 text-tone-info",
  warning: "border-tone-warning/40 bg-tone-warning/10 text-tone-warning",
};

function ToastItem({ toast }: { toast: Toast }) {
  const { removeToast } = useToastContext();
  const Icon = ICONS[toast.type];

  useEffect(() => {
    if (!toast.duration) return;
    const timer = setTimeout(() => {
      removeToast(toast.id);
    }, toast.duration);
    return () => clearTimeout(timer);
  }, [toast.id, toast.duration, removeToast]);

  return (
    <div
      role="status"
      aria-live="polite"
      className={cn(
        "flex items-start gap-3 rounded-lg border bg-card p-4 shadow-lg animate-in slide-in-from-right fade-in duration-200",
        STYLES[toast.type],
      )}
    >
      <Icon className="size-5 shrink-0 mt-0.5" aria-hidden="true" />
      <div className="flex-1 min-w-0">
        <p className="text-sm">{toast.message}</p>
        {toast.action && (
          <Button
            variant="ghost"
            size="sm"
            className="mt-2 h-auto px-2 py-1 text-xs"
            onClick={() => {
              toast.action?.onClick();
              removeToast(toast.id);
            }}
          >
            {toast.action.label}
          </Button>
        )}
      </div>
      <Button
        variant="ghost"
        size="icon"
        className="size-6 shrink-0"
        aria-label="关闭"
        onClick={() => removeToast(toast.id)}
      >
        <X className="size-4" />
      </Button>
    </div>
  );
}

export function Toaster() {
  const { toasts } = useToastContext();

  if (toasts.length === 0) return null;

  return (
    <div
      aria-label="通知"
      className="fixed top-4 right-4 z-50 flex flex-col gap-2 w-80 max-w-[calc(100vw-2rem)]"
    >
      {toasts.map((toast) => (
        <ToastItem key={toast.id} toast={toast} />
      ))}
    </div>
  );
}
