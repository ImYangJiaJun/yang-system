import { screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function stubMe(email: string | null, emailVerifiedAt: number | null) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.endsWith("/.well-known/yang/ui-catalog")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: {
            schema_version: "2.3",
            revision: "a".repeat(64),
            actions: [],
            table_views: [],
            modules: [],
          },
        });
      }
      if (url.endsWith("/api/v1/users/me")) {
        return jsonResponse({
          code: 0,
          data: {
            id: 7,
            username: "alice",
            email,
            email_verified_at: emailVerifiedAt,
            status: "active",
            created_at: 500,
            updated_at: 600,
          },
        });
      }
      if (url.endsWith("/api/v1/users/sessions")) {
        return jsonResponse({ code: 0, data: { sessions: [] } });
      }
      if (url.endsWith("/api/v1/users/security-events")) {
        return jsonResponse({ code: 0, data: { events: [] } });
      }
      throw new Error(`测试未覆盖的请求：${url}`);
    }),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("账号设置页邮箱文案", () => {
  it("空邮箱时显示绑定入口", async () => {
    stubMe(null, null);
    renderTestApp({ path: "/account", authenticated: true });

    await waitFor(() =>
      expect(screen.getByText("未绑定邮箱")).toBeInTheDocument(),
    );
    expect(
      screen.getByRole("heading", { name: "绑定邮箱" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "绑定邮箱" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("向邮箱发送一次性验证码，验证通过后即可用于找回密码"),
    ).toBeInTheDocument();
  });

  it("已验证邮箱时保持更换邮箱", async () => {
    stubMe("a@example.com", 1000);
    renderTestApp({ path: "/account", authenticated: true });

    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: "更换邮箱" }),
      ).toBeInTheDocument(),
    );
    expect(
      screen.getByRole("button", { name: "更换邮箱" }),
    ).toBeInTheDocument();
  });
});
