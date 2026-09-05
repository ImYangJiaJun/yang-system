import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/api/auth-session";
import { SessionController } from "@/api/session-controller";

function tokenResponse(accessToken: string) {
  return new Response(
    JSON.stringify({
      code: 0,
      message: "成功",
      data: { access_token: accessToken },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  );
}

function logoutResponse() {
  return new Response(
    JSON.stringify({
      code: 0,
      message: "已退出",
      data: {
        revoked_all_sessions: true,
        immediate_convergence: true,
        relogin_required: true,
      },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  );
}

beforeEach(() => {
  clearStoredSession();
});

afterEach(() => {
  clearStoredSession();
  sessionStorage.clear();
  localStorage.clear();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("SessionController", () => {
  it("beginSession 通过显式协调动作清空旧 owner 并写入内存 Token", () => {
    const onSessionReset = vi.fn();
    const controller = new SessionController({ onSessionReset });
    sessionStorage.setItem("yang.token", "attacker-controlled-token");

    controller.beginSession({ accessToken: "access-token" });

    expect(onSessionReset).toHaveBeenCalledOnce();
    expect(controller.getSnapshot()).toEqual({
      token: "access-token",
      restoreState: "authenticated",
      loggedIn: true,
    });
    // 旧实现遗留的 Web Storage 凭据必须被清除，只允许内存持有。
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(sessionStorage.getItem("yang.refresh-token")).toBeNull();
    expect(sessionStorage.getItem("yang.account-identity")).toBeNull();
  });

  it("忽略 Web Storage 中注入的旧 Access Token", () => {
    sessionStorage.setItem("yang.token", "attacker-controlled-token");

    const controller = new SessionController();

    expect(controller.getSnapshot().token).toBe("");
    expect(controller.getSnapshot().loggedIn).toBe(false);
    expect(controller.getSnapshot().restoreState).toBe("pending");
  });

  it("acceptRefreshedTokenPair 只轮换 Token，不触发 owner 级联", () => {
    const onSessionReset = vi.fn();
    const controller = new SessionController({ onSessionReset });
    controller.beginSession({ accessToken: "access-old" });
    onSessionReset.mockClear();

    controller.acceptRefreshedTokenPair({ accessToken: "access-new" });

    expect(controller.getSnapshot().token).toBe("access-new");
    expect(onSessionReset).not.toHaveBeenCalled();
  });

  it("页面重载只通过 HttpOnly Refresh Cookie 恢复内存会话", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.credentials).toBe("include");
      return tokenResponse("restored-access");
    });
    vi.stubGlobal("fetch", fetchMock);
    const controller = new SessionController();

    await expect(controller.restoreFromCookie()).resolves.toBe(true);

    expect(controller.getSnapshot()).toMatchObject({
      token: "restored-access",
      restoreState: "authenticated",
      loggedIn: true,
    });
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it("并发恢复共享同一次刷新请求", async () => {
    const fetchMock = vi.fn(async () => tokenResponse("restored-access"));
    vi.stubGlobal("fetch", fetchMock);
    const controller = new SessionController();

    const [first, second] = await Promise.all([
      controller.restoreFromCookie(),
      controller.restoreFromCookie(),
    ]);

    expect(first).toBe(true);
    expect(second).toBe(true);
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it("伪造旧 Token 且 Refresh Cookie 无效时保持未认证并清除上下文", async () => {
    sessionStorage.setItem("yang.token", "forged-access");
    sessionStorage.setItem("yang.account-identity", "user");
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({ code: 40102, message: "刷新会话 Cookie 缺失" }),
            {
              status: 401,
              headers: { "content-type": "application/json" },
            },
          ),
      ),
    );
    const controller = new SessionController();

    await expect(controller.restoreFromCookie()).resolves.toBe(false);

    expect(controller.getSnapshot()).toMatchObject({
      token: "",
      restoreState: "anonymous",
      loggedIn: false,
    });
    expect(sessionStorage.getItem("yang.token")).toBeNull();
    expect(sessionStorage.getItem("yang.account-identity")).toBeNull();
    // 已判定 anonymous 后不再重复请求恢复。
    await expect(controller.restoreFromCookie()).resolves.toBe(false);
  });

  it("clearSession 确定性级联清空会话与 owner", () => {
    const onSessionReset = vi.fn();
    const controller = new SessionController({ onSessionReset });
    controller.beginSession({ accessToken: "access-token" });
    onSessionReset.mockClear();

    controller.clearSession();

    expect(controller.getSnapshot()).toEqual({
      token: "",
      restoreState: "anonymous",
      loggedIn: false,
    });
    expect(onSessionReset).toHaveBeenCalledOnce();
    expect(sessionStorage.length).toBe(0);
  });

  it("subscribe 在状态变化时收到通知，快照引用在两次通知间保持稳定", () => {
    const controller = new SessionController();
    const listener = vi.fn();
    const unsubscribe = controller.subscribe(listener);
    const before = controller.getSnapshot();

    controller.beginSession({ accessToken: "access-token" });

    expect(listener).toHaveBeenCalledOnce();
    expect(controller.getSnapshot()).not.toBe(before);
    expect(controller.getSnapshot()).toBe(controller.getSnapshot());

    unsubscribe();
    controller.clearSession();
    expect(listener).toHaveBeenCalledOnce();
  });

  it("endSession 成功后清空会话并广播多标签页结束信号", async () => {
    vi.stubGlobal("BroadcastChannel", undefined);
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => logoutResponse()),
    );
    const controller = new SessionController();
    controller.beginSession({ accessToken: "access-token" });

    await expect(controller.endSession()).resolves.toBe(true);

    expect(controller.getSnapshot().loggedIn).toBe(false);
    const signalCall = setItem.mock.calls.find(
      ([key]) => key === "yang.session-signal",
    );
    expect(signalCall?.[1]).toContain('"reason":"logout"');
  });

  it("endSession 遇到 Step-up challenge 时用注入回调取 proof 并重放", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      const headers = new Headers(init.headers);
      if (headers.get("x-step-up-proof") !== "one-shot-proof") {
        return new Response(
          JSON.stringify({
            code: 40110,
            message: "敏感操作需要重新认证",
            data: { challenge: "signed-challenge", expires_in: 120 },
          }),
          { status: 428, headers: { "content-type": "application/json" } },
        );
      }
      return logoutResponse();
    });
    vi.stubGlobal("fetch", fetchMock);
    const requestStepUpProof = vi.fn(
      async () => "one-shot-proof" as string | undefined,
    );
    const controller = new SessionController({ requestStepUpProof });
    controller.beginSession({ accessToken: "access-token" });

    await expect(controller.endSession()).resolves.toBe(true);

    expect(requestStepUpProof).toHaveBeenCalledWith("signed-challenge", {
      token: "access-token",
    });
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("用户取消 Step-up 时中止退出并保留会话", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 40110,
              message: "敏感操作需要重新认证",
              data: { challenge: "signed-challenge", expires_in: 120 },
            }),
            { status: 428, headers: { "content-type": "application/json" } },
          ),
      ),
    );
    const controller = new SessionController({
      requestStepUpProof: async () => undefined,
    });
    controller.beginSession({ accessToken: "access-token" });

    await expect(controller.endSession()).resolves.toBe(false);

    expect(controller.getSnapshot().token).toBe("access-token");
  });

  it("未注入 Step-up 回调时遇到 challenge 直接抛错而不是静默跳过", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 40110,
              message: "敏感操作需要重新认证",
              data: { challenge: "signed-challenge", expires_in: 120 },
            }),
            { status: 428, headers: { "content-type": "application/json" } },
          ),
      ),
    );
    const controller = new SessionController();
    controller.beginSession({ accessToken: "access-token" });

    await expect(controller.endSession()).rejects.toMatchObject({
      name: "StepUpRequiredError",
    });
    expect(controller.getSnapshot().token).toBe("access-token");
  });
});
