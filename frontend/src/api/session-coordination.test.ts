import { afterEach, describe, expect, it, vi } from "vitest";
import {
  publishSessionEnd,
  SESSION_SIGNAL_STORAGE_KEY,
  subscribeSessionEnd,
} from "./session-coordination";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

describe("跨标签页会话协调", () => {
  it("只广播会话结束元数据，不把凭据写入 localStorage", () => {
    vi.stubGlobal("BroadcastChannel", undefined);
    const setItem = vi.spyOn(Storage.prototype, "setItem");

    publishSessionEnd("logout");

    expect(setItem).toHaveBeenCalledTimes(1);
    const [key, value] = setItem.mock.calls[0] ?? [];
    expect(key).toBe(SESSION_SIGNAL_STORAGE_KEY);
    expect(value).toContain('"type":"session-ended"');
    expect(value).not.toMatch(/access|refresh|token/i);
    expect(localStorage.getItem(SESSION_SIGNAL_STORAGE_KEY)).toBeNull();
  });

  it("凭据变更使用独立的无敏感信息结束原因", () => {
    vi.stubGlobal("BroadcastChannel", undefined);
    const setItem = vi.spyOn(Storage.prototype, "setItem");

    publishSessionEnd("credentials-changed");

    const [, value] = setItem.mock.calls[0] ?? [];
    expect(value).toContain('"reason":"credentials-changed"');
    expect(value).not.toMatch(/password|access|refresh|token/i);
  });

  it("忽略畸形和伪造消息，只接受版本化结束信号", () => {
    vi.stubGlobal("BroadcastChannel", undefined);
    const listener = vi.fn();
    const dispose = subscribeSessionEnd(listener);

    window.dispatchEvent(
      new StorageEvent("storage", {
        key: SESSION_SIGNAL_STORAGE_KEY,
        newValue: '{"type":"session-ended","reason":"logout","token":"leak"}',
      }),
    );
    window.dispatchEvent(
      new StorageEvent("storage", {
        key: SESSION_SIGNAL_STORAGE_KEY,
        newValue: JSON.stringify({
          version: 1,
          id: "event-1",
          sender: "other-tab",
          type: "session-ended",
          reason: "expired",
        }),
      }),
    );

    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith("expired");
    dispose();
  });
});
