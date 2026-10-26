import { afterEach, expect, it, vi } from "vitest";
import { waitForSignal, withRequestDeadline } from "@/engine/http/deadline";

afterEach(() => vi.useRealTimers());

it("调用方取消同时终止等待与底层请求，即使底层尚未响应取消", async () => {
  const caller = new AbortController();
  let received!: AbortSignal;
  const pending = withRequestDeadline((signal) => {
    received = signal;
    return new Promise<never>(() => {});
  }, caller.signal).catch((error: unknown) => error);
  caller.abort();
  expect(await pending).toMatchObject({ name: "AbortError" });
  expect(received.aborted).toBe(true);
});

it("已取消的调用不启动请求", async () => {
  const controller = new AbortController();
  controller.abort();
  const task = vi.fn(async () => "unexpected");
  await expect(
    withRequestDeadline(task, controller.signal),
  ).rejects.toMatchObject({ name: "AbortError" });
  expect(task).not.toHaveBeenCalled();
});

it("没有调用方signal的共享Promise保持正常完成", async () => {
  await expect(waitForSignal(Promise.resolve("done"))).resolves.toBe("done");
});

it("完成后释放超时定时器", async () => {
  vi.useFakeTimers();
  await expect(withRequestDeadline(async () => "done")).resolves.toBe("done");
  expect(vi.getTimerCount()).toBe(0);
});
