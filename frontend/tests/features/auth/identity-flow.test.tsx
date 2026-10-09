import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { loadStoredIdentity, storeIdentity } from "@/features/auth/identity";
import { renderTestApp } from "@test/helpers/render-app";

/// 身份切换链路：多身份 → /select-identity → 选择 → 导航过滤 → AccountSwitcher 切换。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/// 双身份 Catalog：account.user（user）+ admin.user（admin），无视图无 Action。
const twoIdentityCatalog = {
  code: 0,
  message: "成功",
  data: {
    schema_version: "2.3",
    revision: "e".repeat(64),
    actions: [],
    table_views: [],
    modules: [
      {
        module_id: "account.user",
        identity: { id: "user", title: "个人账号", icon: "person", order: 10 },
        title: "账号中心",
        description: "",
        icon: "account",
        order: 10,
        actions: [],
        action_presentations: [],
        views: [],
      },
      {
        module_id: "admin.user",
        identity: {
          id: "admin",
          title: "平台管理",
          icon: "administrator",
          order: 20,
        },
        title: "平台账号",
        description: "",
        icon: "admin_users",
        order: 20,
        actions: [],
        action_presentations: [],
        views: [],
      },
    ],
  },
};

function stubCatalogFetch(withRoute = false) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes("/.well-known/yang/ui-catalog")) {
        return jsonResponse(
          withRoute
            ? {
                ...twoIdentityCatalog,
                data: {
                  ...twoIdentityCatalog.data,
                  revision: "b".repeat(64),
                  modules: twoIdentityCatalog.data.modules.map((m) => ({
                    ...m,
                    app_route: `/m/${m.module_id}?landing=console`,
                  })),
                },
              }
            : twoIdentityCatalog,
        );
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

describe("身份切换", () => {
  it("Cookie 恢复后 Catalog 只剩一个域时纠正已保存的失效域", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) =>
        input.toString().includes("/refresh")
          ? jsonResponse({
              code: 0,
              message: "成功",
              data: { access_token: "test-access" },
            })
          : jsonResponse({
              ...twoIdentityCatalog,
              data: {
                ...twoIdentityCatalog.data,
                revision: "1".repeat(64),
                modules: [twoIdentityCatalog.data.modules[0]],
              },
            }),
      ),
    );
    storeIdentity("admin");
    const { identityStore } = renderTestApp({
      path: "/",
      authenticated: false,
    });
    expect(
      await within(await screen.findByRole("navigation")).findByRole("link", {
        name: "账号中心",
      }),
    ).toBeInTheDocument();
    await waitFor(() => expect(identityStore.getSnapshot()).toBe("user"));
    expect(loadStoredIdentity()).toBe("user");
    expect(screen.getByTestId("module-card-account.user")).toBeInTheDocument();
  });

  it("最后一个管理权限撤销后更新 Catalog，侧栏与首页恢复到剩余域", async () => {
    stubCatalogFetch();
    const { queryClient, identityStore, router } = renderTestApp({
      path: "/",
    });
    await screen.findByRole("button", { name: "选择平台管理功能域" });
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "选择平台管理功能域" }));
    await act(() => router.navigate("/"));
    const nav = within(screen.getByRole("navigation"));
    await nav.findByRole("link", { name: "平台账号" });
    act(() =>
      queryClient.setQueryData(["ui-catalog", "test-access"], {
        ...twoIdentityCatalog.data,
        revision: "2".repeat(64),
        modules: [twoIdentityCatalog.data.modules[0]],
      }),
    );
    expect(
      await nav.findByRole("link", { name: "账号中心" }),
    ).toBeInTheDocument();
    await waitFor(() => expect(identityStore.getSnapshot()).toBe("user"));
    expect(loadStoredIdentity()).toBe("user");
    expect(
      nav.queryByRole("link", { name: "平台账号" }),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("module-card-account.user")).toBeInTheDocument();
  });

  it("选择功能域与切换器均使用 Catalog app_route", async () => {
    stubCatalogFetch(true);
    const { router } = renderTestApp({ path: "/select-identity" });
    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "选择个人账号功能域" }),
    );
    await waitFor(() =>
      expect(router.state.location.search).toBe("?landing=console"),
    );
    expect(router.state.location.pathname).toBe("/m/account.user");
    await user.click(screen.getByRole("button", { name: "账号菜单" }));
    await user.click(await screen.findByRole("menuitem", { name: "平台管理" }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/m/admin.user"),
    );
    expect(router.state.location.search).toBe("?landing=console");
  });
  it("多身份账号访问首页进入 /select-identity，选择后落到该身份首个模块", async () => {
    stubCatalogFetch();
    const { router } = renderTestApp({ path: "/", authenticated: true });

    // Dashboard 识别多身份未选择 → 选择页。
    expect(
      await screen.findByRole(
        "heading",
        { name: "选择本次使用的功能域" },
        { timeout: 5000 },
      ),
    ).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/select-identity");

    const user = userEvent.setup();
    await user.click(
      screen.getByRole("button", { name: "选择个人账号功能域" }),
    );

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/m/account.user");
    });
    expect(loadStoredIdentity()).toBe("user");
    // 导航只含当前身份模块。
    expect(screen.getByRole("link", { name: "账号中心" })).toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: "平台账号" }),
    ).not.toBeInTheDocument();
  });

  it("AccountSwitcher 切换身份后导航联动过滤", async () => {
    stubCatalogFetch();
    const { router } = renderTestApp({
      path: "/m/account.user",
      authenticated: true,
      identity: "user",
    });

    await screen.findByRole("link", { name: "账号中心" });
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "账号菜单" }));
    await screen.findByRole("menu");
    await user.click(await screen.findByRole("menuitem", { name: "平台管理" }));

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/m/admin.user");
    });
    expect(loadStoredIdentity()).toBe("admin");
    expect(screen.getByRole("link", { name: "平台账号" })).toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: "账号中心" }),
    ).not.toBeInTheDocument();
  });

  it("单身份账号免选择直接进入", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === "string" ? input : input.toString();
        if (url.includes("/.well-known/yang/ui-catalog")) {
          return jsonResponse({
            ...twoIdentityCatalog,
            data: {
              ...twoIdentityCatalog.data,
              // revision 是内容地址：变体必须换 revision，否则命中进程内缓存。
              revision: "f".repeat(64),
              modules: [twoIdentityCatalog.data.modules[0]],
            },
          });
        }
        throw new Error(`测试未覆盖的请求：${url}`);
      }),
    );
    renderTestApp({ path: "/", authenticated: true });

    // 停留在应用中心并自动选中唯一身份。
    expect(
      await screen.findByRole("heading", { name: "应用中心", level: 1 }),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(loadStoredIdentity()).toBe("user");
    });
  });
});
