import {
  useReducer,
  useCallback,
  useMemo,
  createContext,
  useContext,
} from "react";

/**
 * 全局 Toast 通知系统。
 *
 * 设计约束：
 * 1. **瞬时性**：操作结果（成功/失败/信息）不占布局空间，自动消失。
 * 2. **可配置**：每条 toast 可设 duration、action、type。
 * 3. **单一渲染点**：`<Toaster />` 在 App 根挂载，所有组件通过 `useToast()` 调用。
 * 4. **不替代内联校验**：表单字段级错误仍留在输入框旁，toast 只处理操作反馈。
 */

export type ToastType = "success" | "error" | "info" | "warning";

export interface Toast {
  id: string;
  type: ToastType;
  message: string;
  duration?: number;
  action?: {
    label: string;
    onClick: () => void;
  };
}

type ToastAction =
  { type: "ADD"; toast: Toast } | { type: "REMOVE"; id: string };

function toastReducer(state: Toast[], action: ToastAction): Toast[] {
  switch (action.type) {
    case "ADD":
      return [...state, action.toast];
    case "REMOVE":
      return state.filter((t) => t.id !== action.id);
  }
}

interface ToastContextValue {
  toasts: Toast[];
  addToast: (toast: Omit<Toast, "id">) => string;
  removeToast: (id: string) => void;
}

const ToastContext = createContext<ToastContextValue | null>(null);

export function ToastProvider({ children }: { children: React.ReactNode }) {
  const [toasts, dispatch] = useReducer(toastReducer, []);

  const addToast = useCallback((toast: Omit<Toast, "id">): string => {
    const id = `toast-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
    dispatch({ type: "ADD", toast: { ...toast, id } });
    return id;
  }, []);

  const removeToast = useCallback((id: string) => {
    dispatch({ type: "REMOVE", id });
  }, []);

  const value = useMemo(
    () => ({ toasts, addToast, removeToast }),
    [toasts, addToast, removeToast],
  );

  return (
    <ToastContext.Provider value={value}>{children}</ToastContext.Provider>
  );
}

// eslint-disable-next-line react-refresh/only-export-components
export function useToastContext(): ToastContextValue {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    throw new Error("useToastContext 必须在 <ToastProvider> 内调用");
  }
  return ctx;
}
