import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function action(operationId: string) {
  return {
    operation_id: operationId,
    title: operationId,
    description: "",
    method: "POST",
    path: "/api/v1/users/{id}/password-reset-tokens",
    params: [],
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

function catalog(canIssue: boolean) {
  return {
    schema_version: "2.3",
    // revision 相同会被进程内 CatalogCache 当成同一份目录复用。
    revision: (canIssue ? "a" : "b").repeat(64),
    actions: canIssue
      ? [action("account.admin_users.admin_issue_password_reset")]
      : [],
    table_views: [],
    modules: [],
  };
}

function stubUsers(canIssue: boolean) {
  const issue = vi.fn(async () =>
    jsonResponse({
      code: 0,
      data: { reset_token: "once-token", expires_in: 600 },
    }),
  );
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.endsWith("/.well-known/yang/ui-catalog")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: catalog(canIssue),
        });
      }
      if (url.includes("/api/v1/users?") && (init?.method ?? "GET") === "GET") {
        return jsonResponse({
          code: 0,
          data: {
            users: [
              {
                id: 7,
                username: "alice",
                email: null,
                status: "active",
              },
            ],
          },
        });
      }
      if (url.endsWith("/api/v1/users/7/password-reset-tokens")) {
        return issue();
      }
      throw new Error(`测试未覆盖的请求：${url}`);
    }),
  );
  return issue;
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("后台用户列表", () => {
  it("有签发权限时渲染按钮，凭证只出现一次", async () => {
    const issue = stubUsers(true);
    const user = userEvent.setup();
    renderTestApp({ path: "/admin/users", authenticated: true });

    expect(
      await screen.findByRole("button", { name: "签发重置凭证" }),
    ).toBeInTheDocument();
    expect(screen.getByText("alice")).toBeInTheDocument();
    expect(screen.getByText("未绑定")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "签发重置凭证" }));

    expect(await screen.findByText("once-token")).toBeInTheDocument();
    expect(screen.getAllByText("once-token")).toHaveLength(1);
    expect(issue).toHaveBeenCalledOnce();

    await user.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() =>
      expect(screen.queryByText("once-token")).not.toBeInTheDocument(),
    );
  });

  it("没有签发权限时不渲染按钮", async () => {
    stubUsers(false);
    renderTestApp({ path: "/admin/users", authenticated: true });

    expect(await screen.findByText("alice")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "签发重置凭证" }),
    ).not.toBeInTheDocument();
  });
});
