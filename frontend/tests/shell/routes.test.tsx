/**
 * 路由级 lazy 页面的兜底界面。
 *
 * 守的是 2026-09-24 现场那条控制台警告：
 * `No \`HydrateFallback\` element provided to render during initial hydration`。
 *
 * 它不是一句噪音。`createBrowserRouter` 在**构造时**就发现「初始匹配里含 lazy 路由」，
 * 于是把 `renderFallback` 置为 true——也就是说，**硬导航直接落到 lazy 页面**
 * （刷新、从收藏夹打开、从飞书点回来）时，在首次导航完成之前 react-router 需要一个
 * 兜底界面。没有它，它会**把渲染截断到第一个匹配**（只渲染最外层的 SessionBridge），
 * 于是那段窗口里页面是空白的（现场那次抓到过一张 87 字节的空快照）。
 * 应用内点链接（SPA 导航）永远碰不到这条路径，所以它只在「深链接进去」时才现形。
 *
 * 兜底挂在**路由对象上**而不是 lazy 模块里：模块得先加载完才能渲染它的兜底，
 * 那就等于没有兜底。
 */

import { act, render, screen } from "@testing-library/react";
import { Outlet, RouterProvider, createMemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RouteFallback } from "@/shell/RouteFallback";
import { appRoutes } from "@/shell/routes";
import { renderTestApp } from "@test/helpers/render-app";

afterEach(() => {
  vi.restoreAllMocks();
});

/// 让首次导航（含 lazy 模块的动态 import）走完，把之后的渲染也纳入观察。
/// 这里刻意不等某个具体界面：jsdom 里没有后端，AppLayout 的目录查询必然失败，
/// 等界面会把「路由层的行为」和「网络层的行为」搅在一起。
async function settle() {
  await act(async () => {
    await new Promise((resolve) => {
      setTimeout(resolve, 0);
    });
  });
}

describe("硬导航落到 lazy 路由：不再打那条警告", () => {
  it("深链接进数据源列表时不出现 HydrateFallback 警告", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);

    renderTestApp({ path: "/feishu/datasources" });
    await settle();

    expect(warn.mock.calls.flat().map(String).join("\n")).not.toContain(
      "HydrateFallback",
    );
  });

  it("深链接进数据源详情页同样不出现", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);

    renderTestApp({ path: "/feishu/datasources/1" });
    await settle();

    expect(warn.mock.calls.flat().map(String).join("\n")).not.toContain(
      "HydrateFallback",
    );
  });
});

describe("兜底界面的接线", () => {
  it("react-router 在首次导航完成前就渲染出 hydrateFallbackElement", () => {
    // 用一个最小的路由把机制本身钉住：外层布局必须在，lazy 子路由的位置由兜底顶上。
    const router = createMemoryRouter(
      [
        {
          path: "/",
          element: (
            <>
              <span>外壳</span>
              <Outlet />
            </>
          ),
          children: [
            {
              path: "lazy",
              hydrateFallbackElement: <RouteFallback />,
              lazy: async () => ({
                Component: () => <span>真实页面</span>,
              }),
            },
          ],
        },
      ],
      { initialEntries: ["/lazy"] },
    );

    render(<RouterProvider router={router} />);

    // 同步断言：兜底的意义就是「lazy 模块还没到」的那一瞬间也有东西可看
    expect(screen.getByText("外壳")).toBeInTheDocument();
    expect(
      screen.getByRole("status", { name: "页面加载中" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("真实页面")).toBeNull();
  });

  it("每条路由级 lazy 都配了兜底——漏一条就漏一条空白窗口", () => {
    const lazyRoutes: { path?: string; hydrateFallbackElement?: unknown }[] =
      [];
    const walk = (routes: readonly unknown[]) => {
      for (const candidate of routes) {
        const route = candidate as {
          path?: string;
          lazy?: unknown;
          hydrateFallbackElement?: unknown;
          children?: readonly unknown[];
        };
        if (route.lazy) lazyRoutes.push(route);
        if (route.children) walk(route.children);
      }
    };
    walk(appRoutes);

    // 这条断言的前提是「确实存在 lazy 路由」；一条都没有说明上面的遍历写错了，
    // 而不是「全都合规」。
    expect(lazyRoutes.length).toBeGreaterThan(0);
    for (const route of lazyRoutes) {
      // 必须是**那个**兜底组件，不能只要求「非 undefined」：随手写个 `<div />`
      // 也满足「有值」，而那时硬导航会得到一片空白的 main——正是要修的症状。
      const fallback = route.hydrateFallbackElement as
        { type?: unknown } | undefined;
      expect(
        fallback?.type,
        `路由 ${route.path ?? "(无名)"} 的 hydrateFallbackElement 不是 RouteFallback`,
      ).toBe(RouteFallback);
    }
  });

  it("兜底界面自己是有内容、可朗读的，不是又一片空白", () => {
    render(<RouteFallback />);

    expect(
      screen.getByRole("status", { name: "页面加载中" }),
    ).toHaveTextContent("正在加载页面…");
  });
});
