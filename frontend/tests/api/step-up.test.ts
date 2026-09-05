import { afterEach, describe, expect, it, vi } from "vitest";
import { clearStoredSession } from "@/api/auth-session";
import { completeStepUp } from "@/api/step-up";

afterEach(() => {
  clearStoredSession();
  sessionStorage.clear();
  localStorage.clear();
  vi.unstubAllGlobals();
});

describe("completeStepUp", () => {
  it("只向固定端点发送 challenge/凭据并返回内存 proof", async () => {
    const fetchMock = vi.fn(async (url: string, init: RequestInit) => {
      expect(url).toBe("/api/v1/users/step-up/complete");
      expect(new Headers(init.headers).get("authorization")).toBe(
        "Bearer access-token",
      );
      expect(init.credentials).toBe("include");
      expect(JSON.parse(String(init.body))).toEqual({
        challenge: "signed-challenge",
        credentials: { username: "alice", password: "correct-password" },
      });
      return new Response(
        JSON.stringify({
          code: 0,
          message: "成功",
          data: { proof: "one-shot-proof", expires_in: 300 },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      completeStepUp(
        "signed-challenge",
        { username: "alice", password: "correct-password" },
        { token: "access-token" },
      ),
    ).resolves.toEqual({ proof: "one-shot-proof", expiresIn: 300 });
    expect(JSON.stringify({ ...sessionStorage })).not.toContain(
      "one-shot-proof",
    );
    expect(JSON.stringify({ ...localStorage })).not.toContain("one-shot-proof");
  });

  it("拒绝缺 proof 或超出服务端上限的成功响应", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 0,
              data: { proof: "one-shot-proof", expires_in: 601 },
            }),
            { status: 200, headers: { "content-type": "application/json" } },
          ),
      ),
    );

    await expect(
      completeStepUp(
        "signed-challenge",
        { username: "alice", password: "correct-password" },
        {},
      ),
    ).rejects.toThrow("Step-up 响应缺少有效 proof");
  });
});
