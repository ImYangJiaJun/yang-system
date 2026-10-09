import { matchRoutes, type RouteObject } from "react-router";
import { describe, expect, it } from "vitest";
import { appRoutes } from "@/shell/routes";

describe("Catalog 控制台落点", () => {
  it.each([
    "account",
    "access/workspace",
    "feishu/datasources",
    "feishu/approval",
  ])("/%s 匹配实际终端路由而非兜底页", (path) => {
    expect(
      matchRoutes<RouteObject>(appRoutes, `/${path}`)?.at(-1)?.route.path,
    ).toBe(path);
  });
});
