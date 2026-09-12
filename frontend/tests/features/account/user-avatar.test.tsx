import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactElement } from "react";

import { clearStoredSession } from "@/engine/session/auth-session";
import { createSessionController } from "@/engine/session/session-controller";
import { SessionControllerContext } from "@/engine/session/use-session";
import { UserAvatar } from "@/features/account/UserAvatar";

/// UserAvatar 回退逻辑：无版本不发请求、版本寻址拉取、失败/空头像回退默认图。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function renderAvatar(ui: ReactElement) {
  const controller = createSessionController();
  controller.beginSession({ accessToken: "tok-1" });
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <SessionControllerContext.Provider value={controller}>
      <QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>
    </SessionControllerContext.Provider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  clearStoredSession();
});

describe("UserAvatar", () => {
  it("avatarVersion 为 null：直接渲染默认图，不发起请求", () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    renderAvatar(<UserAvatar userId={7} avatarVersion={null} />);

    const img = screen.getByRole("img", { name: "用户头像" });
    expect(img).toHaveAttribute(
      "src",
      expect.stringContaining("avatar-default"),
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("有版本：按 user_id 拉取头像并渲染 data_url", async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      expect(url).toBe("/api/v1/users/avatar?user_id=7");
      return jsonResponse({
        code: 0,
        message: "成功",
        data: { etag: "v-1", data_url: "data:image/webp;base64,QUJD" },
      });
    });
    vi.stubGlobal("fetch", fetchMock);

    renderAvatar(<UserAvatar userId={7} avatarVersion="v-1" />);

    await waitFor(() => {
      expect(screen.getByRole("img", { name: "用户头像" })).toHaveAttribute(
        "src",
        "data:image/webp;base64,QUJD",
      );
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("拉取失败：回退默认图", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse({ code: 50001, message: "服务内部错误" }, 500),
      ),
    );

    renderAvatar(<UserAvatar userId={7} avatarVersion="v-1" />);

    await waitFor(() => {
      expect(screen.getByRole("img", { name: "用户头像" })).toHaveAttribute(
        "src",
        expect.stringContaining("avatar-default"),
      );
    });
  });

  it("响应 data_url 为 null（无头像）：回退默认图", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse({
          code: 0,
          message: "成功",
          data: { etag: null, data_url: null },
        }),
      ),
    );

    renderAvatar(<UserAvatar userId={7} avatarVersion="v-1" />);

    await waitFor(() => {
      expect(screen.getByRole("img", { name: "用户头像" })).toHaveAttribute(
        "src",
        expect.stringContaining("avatar-default"),
      );
    });
  });
});
