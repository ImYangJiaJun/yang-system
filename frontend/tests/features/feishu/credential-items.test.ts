import { describe, expect, it } from "vitest";

import type { DatasourceFieldBinding } from "@/features/feishu/types";
import { credentialItems } from "@/features/feishu/types";

/// 哪些绑定会进拷贝清单、每一行的联动 key 是什么。两条判据漂移的后果都是**静默**的：
/// 前者多一行，运维照着配出来的控件永远取不到选项（`SOURCE_DISABLED`）；
/// 后者给错一个字符，级联要么判不出父值、要么（单参数时）静默取错子集。

function binding(
  overrides: Partial<DatasourceFieldBinding> = {},
): DatasourceFieldBinding {
  return {
    fieldId: "fldA",
    fieldName: "币种/Currency",
    sourceKey: "currency",
    parentFieldId: null,
    enabled: true,
    encryptEnabled: false,
    defaultLocale: "zh_cn",
    ...overrides,
  };
}

describe("credentialItems", () => {
  it("停用的绑定不进清单", () => {
    const items = credentialItems({
      fields: [
        binding(),
        binding({ fieldId: "fldB", sourceKey: "fx_rate", enabled: false }),
      ],
    });
    expect(items.map((item) => item.sourceKey)).toEqual(["currency"]);
  });

  it("字段名缓存为空时原样带出 null，不编一个名字", () => {
    // 首次拉取前服务端还没有解析出当前名字——界面得说「还没解析出来」
    const items = credentialItems({ fields: [binding({ fieldName: null })] });
    expect(items[0]?.fieldName).toBeNull();
  });

  it("轮换时间是「拿不到」（undefined），不是「从未轮换」（null）", () => {
    // 绑定上没有这一列（服务端没投影，或还是旧形状）时只能是不知情。
    const items = credentialItems({ fields: [binding()] });
    expect(items[0]?.tokenRotatedAt).toBeUndefined();
  });

  it("绑定带回了轮换时间就原样带到清单行上（那一列才不再永远是「—」）", () => {
    // `token_rotated_at` 是绑定投影上的一个键。写死 undefined 的后果是
    // 界面上那一列**永远**显示「—」，用户看不到刚换过的凭据是什么时候换的。
    const items = credentialItems({
      fields: [binding({ tokenRotatedAt: 1758000000 })],
    });
    expect(items[0]?.tokenRotatedAt).toBe(1758000000);
  });

  it("服务端明确回 null（从未轮换）与「拿不到」是两回事，中途不许被折平", () => {
    const items = credentialItems({
      fields: [binding({ tokenRotatedAt: null })],
    });
    expect(items[0]?.tokenRotatedAt).toBeNull();
  });

  it("没有绑定就是空清单，不是报错", () => {
    expect(credentialItems({ fields: [] })).toEqual([]);
  });
});

describe("credentialItems：级联父与联动 key", () => {
  const parent = binding({
    fieldId: "fldParent",
    fieldName: "费用类别",
    sourceKey: "expense_category",
  });
  const child = binding({
    fieldId: "fldChild",
    fieldName: "明细科目",
    sourceKey: "expense_subject",
    parentFieldId: "fldParent",
  });

  it("没有父字段的绑定：parent 是 null，界面上不出现「联动 key」那一项", () => {
    const items = credentialItems({ fields: [parent] });
    expect(items[0]?.parent).toBeNull();
  });

  it("有父字段时：联动 key 是**父**的 field_id，不是子自己的、也不是父的 source_key", () => {
    // 服务端比较的是请求里的联动参数键与子绑定上的 parent_field_id，
    // 而 parent_field_id 指向同表里那一条父绑定的 field_id。
    const items = credentialItems({ fields: [parent, child] });
    expect(items[1]?.parent).toEqual({
      linkageKey: "fldParent",
      label: "费用类别",
      enabled: true,
    });
  });

  it("父还没解析出字段名时，标签退回 field_id（自造一个名字会与真实表对不上）", () => {
    const items = credentialItems({
      fields: [binding({ ...parent, fieldName: null }), child],
    });
    expect(items[1]?.parent?.label).toBe("fldParent");
  });

  it("父绑定已停用时标出来——那时这一串 key 填上去也没用", () => {
    // 服务端 `load_parent_source_key` 要求父**启用中**，父停了就按无父处理，
    // 整条级联回退全量。清单上不说这一句，那串 key 会看起来「填上就能用」。
    const items = credentialItems({
      fields: [{ ...parent, enabled: false }, child],
    });
    // 父已停用 → 它自己不进清单，所以子这一行是 items[0]
    expect(items[0]?.parent?.enabled).toBe(false);
    // 子本身启用中，所以仍在清单里——这正是需要这句提醒的场景
    expect(items.map((item) => item.sourceKey)).toEqual(["expense_subject"]);
  });

  it("解析父要在**完整集合**里找：只在筛过的集合里找会把「父已停用」退化成「没有父」", () => {
    // 这是本函数最容易写错的一处：先 filter 再找父，父停用的子绑定会变成
    // parent === null——既不显示联动 key，也不说父为什么停了。
    const items = credentialItems({
      fields: [{ ...parent, enabled: false }, child],
    });
    expect(items[0]?.parent).not.toBeNull();
  });

  it("parent_field_id 是空串或纯空白时按「没有父」处理", () => {
    // `parent_field_id` 是自由文本列，后端每一处消费都先 trim 再判空。
    for (const raw of ["", "   "]) {
      const items = credentialItems({
        fields: [parent, binding({ ...child, parentFieldId: raw })],
      });
      expect(items[1]?.parent).toBeNull();
    }
  });

  it("父指针指向一条不存在的绑定时：key 照给，但标成不可用", () => {
    // 悬空指针（父那一条被删了）与「父被停用」对运维是同一个下一步动作：
    // 回向导把父列修好。所以两者折在同一个 false 里，但 key 不能丢——
    // 它是这一行唯一还能指认「本来该挂谁」的东西。
    const items = credentialItems({
      fields: [binding({ ...child, parentFieldId: "fldGone" })],
    });
    expect(items[0]?.parent).toEqual({
      linkageKey: "fldGone",
      label: "fldGone",
      enabled: false,
    });
  });
});
