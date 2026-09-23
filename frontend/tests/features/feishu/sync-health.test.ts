import { describe, expect, it } from "vitest";

import type { DatasourceItem } from "@/features/feishu/types";
import { syncHealth } from "@/features/feishu/types";

/**
 * 同步健康度的判定。
 *
 * 这几条分支各自对应一种**运维要采取不同动作**的情形，而它们的失效形态是
 * 「界面说一切正常，实际早就没在同步了」——所以每条都要钉住。
 */

function item(overrides: Partial<DatasourceItem> = {}): DatasourceItem {
  return {
    id: 7,
    fields: [],
    title: "汇率",
    status: "active",
    updatedAt: 1758000000,
    ingestMode: "pull",
    bitableBaseToken: "ZoCWb82JQaCCiAspCqbcUvlsnwg",
    bitableTableId: "tblauuOafa4acvT3",
    bitableViewId: null,
    lastPullAt: 1758000000,
    lastSuccessAt: 1758000000,
    consecutiveFailures: 0,
    lastError: null,
    ...overrides,
  };
}

describe("syncHealth", () => {
  it("推送型数据源没有同步状态可看", () => {
    const health = syncHealth(item({ ingestMode: "push" }));
    expect(health.tone).toBe("info");
    expect(health.title).toContain("多维表格推送");
    // 关键：不能把「没有同步状态」说成「同步正常」
    expect(health.title).not.toContain("正常");
  });

  it("停用优先于一切，且不把停用前的失败计数当现状", () => {
    const health = syncHealth(
      item({
        status: "disabled",
        consecutiveFailures: 9,
        lastError: "旧的错误",
      }),
    );
    expect(health.tone).toBe("neutral");
    expect(health.title).toBe("已停用");
  });

  it("坐标不全时判为「不会被拉取」而不是「同步正常」", () => {
    // 这是最容易漏的一支：坐标缺失的源每轮都被服务端跳过，
    // 而它的 consecutiveFailures 恒为 0 —— 只看失败计数会得出「一切正常」。
    //
    // 判据是 **Base Token + 数据表 ID** 两项。**视图不在其中**（可空 = 整表拉取），
    // 而「取数列字段名」曾经在——那是字段级时代的坐标，表级化之后它属于**字段绑定**，
    // 表级行上再也没有这一列。它被留在这里的后果是：这个分支对**每一条**真实数据源
    // 都成立，界面永远在说「坐标不完整，不会被拉取」，而服务端正在正常拉取。
    for (const missing of [
      { bitableBaseToken: null },
      { bitableTableId: null },
      { bitableBaseToken: "   " },
      { bitableTableId: "" },
    ]) {
      const health = syncHealth(item(missing));
      expect(health.tone).toBe("warning");
      expect(health.title).toContain("坐标不完整");
    }
  });

  it("视图为空不算坐标不全——那是「整表拉取」", () => {
    // 反方向也要钉住：把可空的视图算成缺失，会把每一条「不限视图」的源都误报成
    // 拉不起来。这两条一起才说明判据恰好是那两项。
    const health = syncHealth(item({ bitableViewId: null }));
    expect(health.title).not.toContain("坐标不完整");
    expect(health.tone).toBe("positive");
  });

  it("连续失败时给出可归因的判断", () => {
    const health = syncHealth(item({ consecutiveFailures: 3 }));
    expect(health.tone).toBe("warning");
    expect(health.title).toContain("3");
  });

  it("坐标齐备但从未同步过，是「尚未同步」而不是失败", () => {
    const health = syncHealth(
      item({ lastSuccessAt: null, lastPullAt: null, consecutiveFailures: 0 }),
    );
    expect(health.tone).toBe("info");
    expect(health.title).toContain("尚未同步");
  });

  it("同步成功过且无连续失败，才算正常", () => {
    const health = syncHealth(item());
    expect(health.tone).toBe("positive");
    expect(health.title).toBe("同步正常");
  });

  it("失败优先于「尚未成功过」呈现", () => {
    // 两者同时成立（从未成功 + 已有失败），要报失败——那才是运维要处理的事。
    const health = syncHealth(
      item({ lastSuccessAt: null, consecutiveFailures: 2 }),
    );
    expect(health.title).toContain("2");
  });
});
