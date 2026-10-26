/// 覆盖排队、续期、请求与响应体读取；不依赖较新浏览器的 AbortSignal.any/timeout。
export async function withRequestDeadline<T>(
  task: (signal: AbortSignal) => Promise<T>,
  parent?: AbortSignal,
  timeoutMs = 60_000,
): Promise<T> {
  const controller = new AbortController();
  const abort = () => controller.abort(parent?.reason);
  if (parent?.aborted) abort();
  else parent?.addEventListener("abort", abort, { once: true });
  const timer = setTimeout(
    () =>
      controller.abort(
        new DOMException(
          "请求超时，操作结果尚未确认，请先检查结果",
          "TimeoutError",
        ),
      ),
    timeoutMs,
  );
  try {
    controller.signal.throwIfAborted();
    return await waitForSignal(task(controller.signal), controller.signal);
  } finally {
    clearTimeout(timer);
    parent?.removeEventListener("abort", abort);
  }
}

export async function waitForSignal<T>(
  promise: Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  if (!signal) return promise;
  signal.throwIfAborted();
  let abort!: () => void;
  const cancelled = new Promise<never>((_resolve, reject) => {
    abort = () => reject(signal.reason);
    signal.addEventListener("abort", abort, { once: true });
  });
  try {
    return await Promise.race([promise, cancelled]);
  } finally {
    signal.removeEventListener("abort", abort);
  }
}
