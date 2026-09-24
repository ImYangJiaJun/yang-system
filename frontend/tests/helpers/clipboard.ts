/**
 * 剪贴板相关的测试桩。
 *
 * 存在的理由有两条，都是 jsdom 的坑：
 *
 * 1. jsdom **不实现** `document.execCommand`，而它正是降级路径要用的东西——不装桩就测不到。
 * 2. jsdom **不实现** `navigator.clipboard`。这意味着「不装桩」在 jsdom 里等价于
 *    「跑在明文 HTTP 上」，这个巧合很容易被误读：装了桩的用例测的是安全上下文那条路，
 *    而线上出事的恰恰是没桩的那条路。所以这里把两种环境**显式**命名出来，
 *    让用例自己声明它在测哪一个（2026-09-24 的事故就是漏了后者）。
 */

import { vi, type Mock } from "vitest";

/// 装一个可断言的 `document.execCommand` 桩。`result` 可以是定值或每次调用的返回值。
export function stubExecCommand(result: boolean): Mock {
  const execCommand = vi.fn().mockReturnValue(result);
  Object.defineProperty(document, "execCommand", {
    value: execCommand,
    configurable: true,
    writable: true,
  });
  return execCommand;
}

/// 装一个每次调用都执行 `onCall` 的 `execCommand` 桩（用于断言「复制时选中的是什么」）。
export function stubExecCommandWith(onCall: () => boolean): Mock {
  const execCommand = vi.fn().mockImplementation(onCall);
  Object.defineProperty(document, "execCommand", {
    value: execCommand,
    configurable: true,
    writable: true,
  });
  return execCommand;
}

/// 安全上下文：Clipboard API 可用。
export function stubClipboardApi(
  writeText: (value: string) => Promise<void>,
): Mock {
  const spy = vi.fn().mockImplementation(writeText);
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText: spy } });
  return spy;
}

/// 非安全上下文（`http://<公网IP>:<端口>`）：Clipboard API **整块缺席**。
/// 注意它不是「拒绝」而是 `undefined`，所以抛出来的是 TypeError 而不是可读的 NotAllowedError。
///
/// ⚠️ 必须在 `userEvent.setup()` **之后**调用：user-event 自己会给 `navigator.clipboard`
/// 装一个可用的实现（为了它的 `copy`/`paste` 能工作），先装会被它覆盖掉，于是
/// 「复制在非安全上下文会失败」根本没被模拟到——测试会以「复制成功」这种假象通过。
export function stubInsecureClipboard(): void {
  vi.stubGlobal("navigator", { ...navigator, clipboard: undefined });
}

/// 撤掉所有桩，恢复成 jsdom 的原样（即「两条路都没有」的状态）。
export function restoreClipboard(): void {
  vi.unstubAllGlobals();
  Reflect.deleteProperty(document, "execCommand");
}
