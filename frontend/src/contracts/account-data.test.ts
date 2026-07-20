import { describe, expect, it } from "vitest";
import { ContractError } from "./ui-catalog";
import { parseOrganizationsPage } from "./account-data";

describe("account data", () => {
  it("把我的企业响应解析为可展示的账号选项", () => {
    expect(
      parseOrganizationsPage({
        items: [
          { id: 7, name: "示例企业", code: "ACME" },
          { id: 9, name: "第二企业", code: "SECOND" },
        ],
        total: 2,
        page: 1,
        limit: 100,
        total_pages: 1,
      }),
    ).toEqual([
      { id: 7, name: "示例企业", code: "ACME" },
      { id: 9, name: "第二企业", code: "SECOND" },
    ]);
  });

  it("拒绝缺少名称或非法 ID 的企业响应", () => {
    expect(() =>
      parseOrganizationsPage({ items: [{ id: "manual", code: "ACME" }] }),
    ).toThrow(ContractError);
  });
});
