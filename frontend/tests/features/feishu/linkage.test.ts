import { describe, expect, it } from "vitest";

import {
  LINKAGE_WILDCARD_KEY,
  buildLinkageMapping,
  parseLinkageMapping,
} from "@/features/feishu/types";

/**
 * 级联声明的编解码。
 *
 * 这一层的失效形态很隐蔽：解错或拼错**不会报错**，只会让该数据源静默退化成
 * 「无级联」——用户看到的是「选完父级之后子级还返回全部」，而不是一条错误。
 */

describe("parseLinkageMapping", () => {
  it("解析出两个必填成员", () => {
    const parsed = parseLinkageMapping(
      '{"widget1":{"parent_source_key":"payment_currency","parent_field":"币种/Currency（单选）"}}',
    );
    expect(parsed).toEqual({
      parentSourceKey: "payment_currency",
      parentField: "币种/Currency（单选）",
      widgetCode: "widget1",
    });
  });

  it("存量数据里的 cascade_field 被忽略而不是判失败", () => {
    // 后端去掉该成员时约定「忽略而非拒绝」，否则存量配置会突然失效
    const parsed = parseLinkageMapping(
      '{"w":{"parent_source_key":"c","parent_field":"p","cascade_field":"子"}}',
    );
    expect(parsed?.parentSourceKey).toBe("c");
    expect(parsed?.parentField).toBe("p");
  });

  it("通配键回到界面上是空串", () => {
    // 用户看到的是「留空」，不是一个星号
    const parsed = parseLinkageMapping(
      `{"${LINKAGE_WILDCARD_KEY}":{"parent_source_key":"c","parent_field":"p"}}`,
    );
    expect(parsed?.widgetCode).toBe("");
  });

  it("解析不出的一律视为无级联", () => {
    for (const raw of [
      null,
      "",
      "   ",
      "{not json",
      "[]",
      '"text"',
      "{}",
      // 缺成员
      '{"w":{"parent_source_key":"c"}}',
      '{"w":{"parent_field":"p"}}',
      '{"w":{"parent_source_key":"  ","parent_field":"  "}}',
      // 条目不是对象
      '{"w":"nope"}',
    ]) {
      expect(parseLinkageMapping(raw)).toBeNull();
    }
  });

  it("只取第一条", () => {
    const parsed = parseLinkageMapping(
      '{"a":{"parent_source_key":"x","parent_field":"p"},"b":{"parent_source_key":"y","parent_field":"q"}}',
    );
    expect(parsed?.parentSourceKey).toBe("x");
  });
});

describe("buildLinkageMapping", () => {
  it("两个成员齐备时拼出后端要的形状", () => {
    const raw = buildLinkageMapping({
      parentSourceKey: "payment_currency",
      parentField: "币种/Currency（单选）",
      widgetCode: "widget1",
    });
    expect(JSON.parse(raw)).toEqual({
      widget1: {
        parent_source_key: "payment_currency",
        parent_field: "币种/Currency（单选）",
      },
    });
    // 已废弃的成员不得再写出去
    expect(raw).not.toContain("cascade_field");
  });

  it("控件代码留空时写通配键", () => {
    const raw = buildLinkageMapping({
      parentSourceKey: "c",
      parentField: "p",
      widgetCode: "",
    });
    expect(JSON.parse(raw)).toEqual({
      [LINKAGE_WILDCARD_KEY]: { parent_source_key: "c", parent_field: "p" },
    });
  });

  it("只留空白的控件代码也算通配", () => {
    const raw = buildLinkageMapping({
      parentSourceKey: "c",
      parentField: "p",
      widgetCode: "   ",
    });
    expect(Object.keys(JSON.parse(raw))).toEqual([LINKAGE_WILDCARD_KEY]);
  });

  it("缺成员时产出空串（即不写这一项）", () => {
    expect(buildLinkageMapping(null)).toBe("");
    expect(
      buildLinkageMapping({
        parentSourceKey: "",
        parentField: "p",
        widgetCode: "",
      }),
    ).toBe("");
    expect(
      buildLinkageMapping({
        parentSourceKey: "c",
        parentField: "  ",
        widgetCode: "",
      }),
    ).toBe("");
  });

  it("与 parse 往返一致", () => {
    const original = {
      parentSourceKey: "payment_currency",
      parentField: "币种/Currency（单选）",
      widgetCode: "",
    };
    expect(parseLinkageMapping(buildLinkageMapping(original))).toEqual(
      original,
    );
  });
});
