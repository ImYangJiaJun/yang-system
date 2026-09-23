import { describe, expect, it } from "vitest";

import type { DatasourceFieldBinding } from "@/features/feishu/types";
import { credentialItems } from "@/features/feishu/types";

/// 哪些绑定会进拷贝清单：只列启用中的。这条判据漂移的后果是**静默**的
/// ——清单上多一行，运维照着配出来的控件永远取不到选项（`SOURCE_DISABLED`）。

function binding(
  overrides: Partial<DatasourceFieldBinding> = {},
): DatasourceFieldBinding {
  return {
    fieldId: "fldA",
    fieldName: "币种/Currency",
    sourceKey: "currency",
    parentFieldId: null,
    enabled: true,
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
    // 列表端点的绑定投影里没有 token_rotated_at，所以这里只能是不知情。
    const items = credentialItems({ fields: [binding()] });
    expect(items[0]?.tokenRotatedAt).toBeUndefined();
  });

  it("没有绑定就是空清单，不是报错", () => {
    expect(credentialItems({ fields: [] })).toEqual([]);
  });
});
