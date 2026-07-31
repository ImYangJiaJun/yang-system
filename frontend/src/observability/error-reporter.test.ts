import { describe, expect, it, vi } from "vitest";
import { ApiError } from "src/api/errors";
import {
  createFrontendErrorReporter,
  type FrontendErrorReport,
} from "./error-reporter";

const RELATED_REQUEST_ID = "0123456789abcdef0123456789abcdef";

describe("frontend error reporter", () => {
  it("上报可关联 API 错误且不泄漏错误正文、详情或 Token", async () => {
    const reports: Array<{
      report: FrontendErrorReport;
      token: string;
    }> = [];
    const reporter = createFrontendErrorReporter({
      accessToken: () => "memory-only-token",
      routeName: () => "module",
      eventId: () => "11111111-1111-4111-8111-111111111111",
      now: () => 1_000,
      send: async (report, token) => {
        reports.push({ report, token });
      },
    });

    const accepted = await reporter.capture(
      new ApiError("不得上报的敏感错误正文 token=secret", {
        status: 503,
        code: 500001,
        requestId: RELATED_REQUEST_ID,
        details: { password: "never-report-me" },
      }),
      { kind: "api", operation: "demo.items.list" },
    );

    expect(accepted).toBe(true);
    expect(reports).toEqual([
      {
        token: "memory-only-token",
        report: {
          event_id: "11111111-1111-4111-8111-111111111111",
          kind: "api",
          route: "module",
          operation: "demo.items.list",
          related_request_id: RELATED_REQUEST_ID,
          status: 503,
          error_code: 500001,
          fingerprint: expect.stringMatching(/^[0-9a-f]{16}$/),
        },
      },
    ]);
    expect(JSON.stringify(reports)).not.toContain("敏感错误正文");
    expect(JSON.stringify(reports)).not.toContain("never-report-me");
  });

  it("无会话时 fail-closed，且相同指纹在冷却窗口内只发送一次", async () => {
    const send = vi.fn(async () => undefined);
    let token: string | undefined = undefined;
    let now = 2_000;
    const reporter = createFrontendErrorReporter({
      accessToken: () => token,
      routeName: () => "business",
      eventId: () => "22222222-2222-4222-8222-222222222222",
      now: () => now,
      send,
    });
    const error = new Error("重复错误");

    expect(await reporter.capture(error, { kind: "runtime" })).toBe(false);
    token = "active-token";
    expect(await reporter.capture(error, { kind: "runtime" })).toBe(true);
    expect(await reporter.capture(error, { kind: "runtime" })).toBe(false);
    now += 10_001;
    expect(await reporter.capture(error, { kind: "runtime" })).toBe(true);
    expect(send).toHaveBeenCalledTimes(2);
  });

  it("只接受有界路由、operation 和后端 request id", async () => {
    const send = vi.fn(async () => undefined);
    const reporter = createFrontendErrorReporter({
      accessToken: () => "active-token",
      routeName: () => "../module?token=secret",
      eventId: () => "33333333-3333-4333-8333-333333333333",
      now: () => 3_000,
      send,
    });

    await reporter.capture(
      new ApiError("错误", {
        status: 500,
        requestId: "not-a-request-id",
      }),
      { kind: "api", operation: "unsafe/operation?secret=true" },
    );

    expect(send).toHaveBeenCalledWith(
      {
        event_id: "33333333-3333-4333-8333-333333333333",
        kind: "api",
        route: "unknown",
        fingerprint: expect.stringMatching(/^[0-9a-f]{16}$/),
        status: 500,
      },
      "active-token",
    );
  });

  it("契约错误可显式关联响应 request id，且敏感正文变化不改变指纹", async () => {
    const reports: FrontendErrorReport[] = [];
    const reporter = createFrontendErrorReporter({
      accessToken: () => "active-token",
      routeName: () => "module-page",
      eventId: () => "44444444-4444-4444-8444-444444444444",
      now: () => 4_000,
      send: async (report) => {
        reports.push(report);
      },
    });

    expect(
      await reporter.capture(new Error("token=first-secret"), {
        kind: "contract",
        operation: "demo.items.list",
        relatedRequestId: RELATED_REQUEST_ID.toUpperCase(),
      }),
    ).toBe(true);
    const forgedNameError = new Error("token=second-secret");
    forgedNameError.name = "token=forged-error-name";
    expect(
      await reporter.capture(forgedNameError, {
        kind: "contract",
        operation: "demo.items.list",
        relatedRequestId: RELATED_REQUEST_ID,
      }),
    ).toBe(false);
    expect(reports).toEqual([
      expect.objectContaining({
        related_request_id: RELATED_REQUEST_ID,
        fingerprint: expect.stringMatching(/^[0-9a-f]{16}$/),
      }),
    ]);
  });

  it("不支持 randomUUID 的目标浏览器仍生成 RFC 4122 v4 事件标识", async () => {
    const originalCrypto = globalThis.crypto;
    vi.stubGlobal("crypto", {
      getRandomValues: (bytes: Uint8Array) => {
        bytes.fill(0xab);
        return bytes;
      },
    });
    const send = vi.fn(async () => undefined);
    try {
      const reporter = createFrontendErrorReporter({
        accessToken: () => "active-token",
        routeName: () => "module-page",
        now: () => 5_000,
        send,
      });
      await reporter.capture(new Error("旧浏览器"), { kind: "runtime" });
      expect(send).toHaveBeenCalledWith(
        expect.objectContaining({
          event_id: "abababab-abab-4bab-abab-abababababab",
        }),
        "active-token",
      );
    } finally {
      vi.stubGlobal("crypto", originalCrypto);
    }
  });
});
