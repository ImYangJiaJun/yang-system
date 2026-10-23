import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { SessionController } from "@/engine/session/session-controller";

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

function unauthorizedResponse() {
  return new Response(
    JSON.stringify({ code: 40102, message: "Token 已过期" }),
    { status: 401, headers: { "content-type": "application/json" } },
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

  it("Access Token 过期后登出仍清空本地，并用刷新结果重试服务端", async () => {
    vi.stubGlobal("BroadcastChannel", undefined);
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    const calls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        if (url.includes("/api/v1/users/refresh"))
          return tokenResponse("access-refreshed");
        if (url.includes("/api/v1/users/logout")) {
          const authorization = new Headers(init.headers).get("authorization");
          calls.push(`logout:${authorization}`);
          return authorization === "Bearer access-expired"
            ? unauthorizedResponse()
            : logoutResponse();
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );
    const controller = new SessionController();
    controller.beginSession({ accessToken: "access-expired" });

    await expect(controller.endSession()).resolves.toBe(true);

    expect(calls).toEqual([
      "logout:Bearer access-expired",
      "logout:Bearer access-refreshed",
    ]);
    expect(controller.getSnapshot().loggedIn).toBe(false);
    const signalCall = setItem.mock.calls.find(
      ([key]) => key === "yang.session-signal",
    );
    expect(signalCall?.[1]).toContain('"reason":"logout"');
  });

  it("服务端不可达时登出/停用仍清空本地会话", async () => {
    vi.stubGlobal("BroadcastChannel", undefined);
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new Error("offline");
      }),
    );
    const logoutController = new SessionController();
    logoutController.beginSession({ accessToken: "access-token" });
    await expect(logoutController.endSession()).rejects.toThrow("offline");
    expect(logoutController.getSnapshot().loggedIn).toBe(false);

    const disableController = new SessionController();
    disableController.beginSession({ accessToken: "access-token" });
    await expect(disableController.disableAccount()).rejects.toThrow("offline");
    expect(disableController.getSnapshot().loggedIn).toBe(false);

    const logoutSignals = setItem.mock.calls.filter(
      ([key, value]) =>
        key === "yang.session-signal" &&
        String(value).includes('"reason":"logout"'),
    );
    expect(logoutSignals).toHaveLength(2);
  });

  it("恢复超时按可重试错误收敛为匿名，不永久停在 pending", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new DOMException("The operation timed out.", "TimeoutError");
      }),
    );
    const controller = new SessionController();

    await expect(controller.restoreFromCookie()).resolves.toBe(false);

    expect(controller.getSnapshot()).toMatchObject({
      token: "",
      restoreState: "anonymous",
      loggedIn: false,
    });
  });
});
