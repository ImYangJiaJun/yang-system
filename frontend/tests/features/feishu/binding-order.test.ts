import { describe, expect, it } from "vitest";

import type { DatasourceFieldBinding } from "@/features/feishu/types";
import { orderBindingsForDisplay } from "@/features/feishu/types";

/**
 * 「父子在一起」的排序。
 *
 * 这一条是整个 ② 的支点：一条数据源的字段级联是**同表内**的父子链，所以只要排序
 * 正确，「像表格一样看整表」与「父子相邻」就同时成立。失效形态有两种，且都不会
 * 报错——**行序乱七八糟**（人对照不了飞书那张表），或者**整棵子树消失**（更坏：
 * 界面上少了一条真实存在的绑定，而没有任何提示）。
 */

function binding(
  fieldId: string,
  parentFieldId: string | null = null,
): DatasourceFieldBinding {
  return {
    fieldId,
    fieldName: fieldId,
    sourceKey: fieldId.toLowerCase(),
    parentFieldId,
    enabled: true,
    encryptEnabled: false,
    defaultLocale: "zh_cn",
    tokenRotatedAt: null,
  };
}

function order(bindings: DatasourceFieldBinding[]) {
  return orderBindingsForDisplay(bindings).map(({ binding, depth }) => [
    binding.fieldId,
    depth,
  ]);
}

describe("orderBindingsForDisplay", () => {
  it("父在前，子紧随其后并按深度缩进（三级链）", () => {
    // 目标台账上真有三级：费用大类 → 费用类型 → 银行流水摘要-编码。
    // 输入刻意打乱（先给孙、再给子、最后给父），排序必须自己把它理出来。
    const rows = order([
      binding("fldCode", "fldType"),
      binding("fldType", "fldCat"),
      binding("fldCat"),
    ]);
    expect(rows).toEqual([
      ["fldCat", 0],
      ["fldType", 1],
      ["fldCode", 2],
    ]);
  });

  it("同一父下的次序保持后端给的顺序（不重排）", () => {
    // 重排会让运维每次刷新看到不同的行序，而这一栏是用来对照飞书那张表的。
    const rows = order([
      binding("fldB", "fldA"),
      binding("fldC", "fldA"),
      binding("fldA"),
    ]);
    expect(rows.map(([id]) => id)).toEqual(["fldA", "fldB", "fldC"]);
  });

  it("父不在集合里时当成根：整棵子树仍然完整露面", () => {
    // 父列被取消勾选（或没勾）就属于这种。直接跳过它不是「排得不好看」，
    // 而是**把一条真实存在的绑定从界面上抹掉**。
    const rows = order([binding("fldChild", "fldMissing"), binding("fldRoot")]);
    expect(rows).toEqual([
      ["fldChild", 0],
      ["fldRoot", 0],
    ]);
  });

  it("环不会死循环，也不会漏行或重复行", () => {
    // 建源侧有校验，但历史行与并发写都可能留下环。要求：每一行恰好出现一次。
    const rows = order([
      binding("fldX", "fldY"),
      binding("fldY", "fldX"),
      binding("fldRoot"),
    ]);
    expect(rows.map(([id]) => id).sort()).toEqual(["fldRoot", "fldX", "fldY"]);
    expect(rows).toHaveLength(3);
  });

  it("空集合得到空结果（不是抛错）", () => {
    expect(orderBindingsForDisplay([])).toEqual([]);
  });
});
