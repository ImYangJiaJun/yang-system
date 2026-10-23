import { render } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  clearStoredSession,
  requestWithTokenRefresh,
  SESSION_EXPIRED_EVENT,
} from "@/engine/session/auth-session";
import { createSessionController } from "@/engine/session/session-controller";
import { SessionControllerContext } from "@/engine/session/use-session";
import SessionBridge from "@/shell/session-bridge";

/**
 * P0-11 回归：自动刷新得到的新 Access Token 必须回写 SessionController。
 * 否则 useSessionCredentials 的快照永远停在登录/恢复时的 T0，第二个 TTL
 * 到期后请求带着过期的 T1 重试并被强制登出。
 */

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

function renderBridge(controller: ReturnType<typeof createSessionController>) {
  const router = createMemoryRouter([
    {
      element: <SessionBridge />,
      children: [{ index: true, element: <div /> }],
    },
  ]);
  render(
    <SessionControllerContext.Provider value={controller}>
      <RouterProvider router={router} />
    </SessionControllerContext.Provider>,
  );
}

afterEach(() => {
  clearStoredSession();
  sessionStorage.clear();
  localStorage.clear();
  vi.unstubAllGlobals();
});

describe("SessionBridge 刷新回写", () => {
  it("刷新成功后更新控制器快照，后续请求不再携带 T0", async () => {
    const controller = createSessionController();
    controller.beginSession({ accessToken: "access-t0" });
    renderBridge(controller);
    const refresh = vi.fn(async () => tokenResponse("access-t1"));
    vi.stubGlobal("fetch", refresh);
    const seen: Array<string | undefined> = [];
    const request = vi.fn(async (accessToken?: string) => {
      seen.push(accessToken);
      return new Response(undefined, {
        status: accessToken === "access-t0" ? 401 : 200,
      });
    });

    await expect(
      requestWithTokenRefresh(controller.getSnapshot().token, request),
    ).resolves.toMatchObject({ status: 200 });

    expect(seen).toEqual(["access-t0", "access-t1"]);
    expect(controller.getSnapshot().token).toBe("access-t1");

    // 后续请求以控制器快照为源：直接命中新 Token，不再刷新。
    await requestWithTokenRefresh(controller.getSnapshot().token, request);
    expect(seen).toEqual(["access-t0", "access-t1", "access-t1"]);
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("新 Token 再次过期时仍能再次刷新，不触发强制登出", async () => {
    const controller = createSessionController();
    controller.beginSession({ accessToken: "access-t0" });
    renderBridge(controller);
    const refresh = vi
      .fn()
      .mockResolvedValueOnce(tokenResponse("access-t1"))
      .mockResolvedValueOnce(tokenResponse("access-t2"));
    vi.stubGlobal("fetch", refresh);
    const expired = vi.fn();
    window.addEventListener(SESSION_EXPIRED_EVENT, expired);
    const seen: Array<string | undefined> = [];
    let attempt = 0;
    const request = vi.fn(async (accessToken?: string) => {
      seen.push(accessToken);
      attempt += 1;
      return new Response(undefined, {
        status: attempt === 1 || attempt === 3 ? 401 : 200,
      });
    });

    await expect(
      requestWithTokenRefresh(controller.getSnapshot().token, request),
    ).resolves.toMatchObject({ status: 200 });
    await expect(
      requestWithTokenRefresh(controller.getSnapshot().token, request),
    ).resolves.toMatchObject({ status: 200 });

    expect(seen).toEqual(["access-t0", "access-t1", "access-t1", "access-t2"]);
    expect(controller.getSnapshot().token).toBe("access-t2");
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(expired).not.toHaveBeenCalled();
    window.removeEventListener(SESSION_EXPIRED_EVENT, expired);
  });
});
