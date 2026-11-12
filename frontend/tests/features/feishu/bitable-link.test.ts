import { describe, expect, it } from "vitest";

import { parseBitableLink } from "@/features/feishu/types";

describe("parseBitableLink", () => {
  it("按路径和查询参数解析完整链接，不限制 host", () => {
    expect(
      parseBitableLink(
        "https://any-tenant.example/base/ZoCWb82JQaCCiAspCqbcUvlsnwg?table=tblfFjlSuIQeMCJF&view=vewAEKSbvO",
      ),
    ).toEqual({
      baseToken: "ZoCWb82JQaCCiAspCqbcUvlsnwg",
      tableId: "tblfFjlSuIQeMCJF",
      viewId: "vewAEKSbvO",
      origin: "https://any-tenant.example",
    });
  });

  it("缺少 table/view 时保留可解析的坐标", () => {
    expect(parseBitableLink("https://tenant/base/appToken")).toEqual({
      baseToken: "appToken",
      tableId: "",
      viewId: "",
      origin: "https://tenant",
    });
    expect(parseBitableLink("https://tenant/base/appToken?table=tblA")).toEqual(
      {
        baseToken: "appToken",
        tableId: "tblA",
        viewId: "",
        origin: "https://tenant",
      },
    );
  });

  it("裸 token 原样返回且不带 origin", () => {
    expect(parseBitableLink("appToken")).toEqual({
      baseToken: "appToken",
      tableId: "",
      viewId: "",
      origin: "",
    });
  });

  it("拒绝非 Base 路径和非法坐标", () => {
    expect(
      parseBitableLink("https://tenant/wiki/appToken?table=tblA"),
    ).toBeNull();
    expect(
      parseBitableLink("https://tenant/base/appToken?table=../evil"),
    ).toBeNull();
    expect(
      parseBitableLink("https://tenant/base/appToken?table=tblA&table=tblB"),
    ).toBeNull();
  });
});
