import { beforeEach, describe, expect, it } from "vitest";

import {
  loadVisibleColumns,
  saveVisibleColumns,
  setColumnVisible,
} from "@/engine/renderers/table/column-preferences";

beforeEach(() => localStorage.clear());

describe("列显示偏好", () => {
  it("默认全部可见，保存后按视图维度恢复", () => {
    const all = ["id", "name", "status"];
    expect(loadVisibleColumns("demo.items.main", all)).toEqual(all);

    saveVisibleColumns("demo.items.main", ["name"]);
    expect(loadVisibleColumns("demo.items.main", all)).toEqual(["name"]);
    // 视图隔离：其他视图不受影响。
    expect(loadVisibleColumns("demo.other", all)).toEqual(all);
  });

  it("存储损坏或字段越界时回退全量列", () => {
    localStorage.setItem("yang.column-prefs.v1.demo.items.main", "not-json");
    expect(loadVisibleColumns("demo.items.main", ["id"])).toEqual(["id"]);

    saveVisibleColumns("demo.items.main", ["ghost"]);
    expect(loadVisibleColumns("demo.items.main", ["id", "name"])).toEqual([
      "id",
      "name",
    ]);

    saveVisibleColumns("demo.items.main", []);
    expect(loadVisibleColumns("demo.items.main", ["id"])).toEqual(["id"]);
  });

  it("隐藏时至少保留一列，恢复已隐藏列保持顺序追加", () => {
    expect(setColumnVisible(["id", "name"], "name", false)).toEqual(["id"]);
    // 仅剩一列时不允许再隐藏。
    expect(setColumnVisible(["id"], "id", false)).toEqual(["id"]);
    expect(setColumnVisible(["id"], "name", true)).toEqual(["id", "name"]);
    // 重复显示不产生重复项。
    expect(setColumnVisible(["id"], "id", true)).toEqual(["id"]);
  });
});
