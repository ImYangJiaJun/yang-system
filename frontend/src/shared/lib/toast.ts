import {
  useToastContext,
  type ToastType,
} from "@/shared/providers/toast-provider";

/**
 * Toast 通知 Hook。
 *
 * 用法：
 * ```tsx
 * const toast = useToast();
 * toast.success("数据源已创建");
 * toast.error("凭据校验失败", { duration: 10000 });
 * toast.info("验证码已发送", { action: { label: "重发", onClick: resend } });
 * ```
 *
 * 默认 duration：
 * - success: 3000ms
 * - error: 5000ms（错误通常需要更长时间阅读）
 * - info: 3000ms
 * - warning: 4000ms
 */

const DEFAULT_DURATIONS: Record<ToastType, number> = {
  success: 3000,
  error: 5000,
  info: 3000,
  warning: 4000,
};

export function useToast() {
  const { addToast } = useToastContext();

  const show = (
    type: ToastType,
    message: string,
    options?: {
      duration?: number;
      action?: { label: string; onClick: () => void };
    },
  ) => {
    const duration = options?.duration ?? DEFAULT_DURATIONS[type];
    return addToast({ type, message, duration, action: options?.action });
  };

  return {
    success: (
      message: string,
      options?: {
        duration?: number;
        action?: { label: string; onClick: () => void };
      },
    ) => show("success", message, options),
    error: (
      message: string,
      options?: {
        duration?: number;
        action?: { label: string; onClick: () => void };
      },
    ) => show("error", message, options),
    info: (
      message: string,
      options?: {
        duration?: number;
        action?: { label: string; onClick: () => void };
      },
    ) => show("info", message, options),
    warning: (
      message: string,
      options?: {
        duration?: number;
        action?: { label: string; onClick: () => void };
      },
    ) => show("warning", message, options),
  };
}
