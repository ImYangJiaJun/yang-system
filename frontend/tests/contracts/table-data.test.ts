import { describe, expect, it } from "vitest";
import {
  buildTreeRows,
  parseRelationOptions,
  parseTableData,
} from "@/contracts/table-data";

describe("TableView runtime contracts", () => {
  it("只接受标准 items 分页结构", () => {
    expect(
      parseTableData({ items: [{ id: 1 }], page: 1, page_size: 20, total: 1 }),
    ).toMatchObject({ total: 1 });
    expect(() =>
      parseTableData({ data: [{ id: 1 }], page: 1, page_size: 20 }),
    ).toThrow("TableView 数据契约校验失败");
  });

  it("关系 options 只接受标量 value 与稳定 label", () => {
    expect(
      parseRelationOptions({
        items: [{ value: 7, label: "Alice" }],
        page: 1,
        limit: 20,
        total: 1,
      }).items[0],
    ).toEqual({ value: 7, label: "Alice" });
    expect(() =>
      parseRelationOptions({
        items: [{ value: { id: 7 }, label: "Alice" }],
        page: 1,
        limit: 20,
      }),
    ).toThrow("关系 options 契约校验失败");
  });

  it("把无序扁平节点构造成树并拒绝循环", () => {
    const tree = {
      id_field: "id",
      parent_field: "parent_id",
      label_field: "name",
      max_nodes: 10,
    };
    const roots = buildTreeRows(
      [
        { id: 2, parent_id: 1, name: "child" },
        { id: 1, parent_id: null, name: "root" },
      ],
      tree,
    );
    expect(roots[0]?.children?.[0]?.name).toBe("child");
    expect(() =>
      buildTreeRows(
        [
          { id: 1, parent_id: 2, name: "one" },
          { id: 2, parent_id: 1, name: "two" },
        ],
        tree,
      ),
    ).toThrow("循环父子关系");
  });
});
