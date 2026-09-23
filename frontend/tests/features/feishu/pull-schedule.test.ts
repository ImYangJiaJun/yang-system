/**
 * 「下次自动拉取」与「接口地址」两个纯函数的规则。
 *
 * 它们都放在 `types.ts` 而不是页面里，理由与 `syncHealth` 一致：判定分支各对应一种
 * 用户要采取的不同动作，埋在 JSX 里就只能靠肉眼看，而它们恰恰最容易在改动中被无声破坏。
 */

import { describe, expect, it } from "vitest";

import {
  approvalOptionsUrl,
  describeNextPull,
  pullLanded,
  type DatasourceItem,
  type PullScheduleInfo,
} from "@/features/feishu/types";

const NOW = 1_700_000_000;

function schedule(
  nextRunAt: number | null,
  intervalSeconds = 900,
): PullScheduleInfo {
  return { intervalSeconds, nextRunAt };
}

function item(overrides: Partial<DatasourceItem> = {}): DatasourceItem {
  return {
    id: 7,
    fields: [],
    sourceKey: "fx_rate",
    title: "汇率",
    encryptEnabled: false,
    defaultLocale: "zh_cn",
    status: "active",
    updatedAt: 1758000000,
    ingestMode: "pull",
    bitableBaseToken: "ZoCWb82JQaCCiAspCqbcUvlsnwg",
    bitableTableId: "tblauuOafa4acvT3",
    bitableViewId: null,
    bitableFieldName: "币种/Currency（单选）",
    linkageMapping: null,
    lastPullAt: 1758000000,
    lastSuccessAt: 1758000000,
    consecutiveFailures: 0,
    lastError: null,
    snapshotDigest: "abc",
    ...overrides,
  };
}

describe("describeNextPull", () => {
  it("没有排程时不编一个假时间出来", () => {
    // 控制台查不到排程（出站拉取没启用、目录里没这个 Action）时只能承认不知道。
    const view = describeNextPull(null, NOW);
    expect(view.label).toBe("未知");
  });

  it("正在拉取时不说一个已经过去或即将到期的时刻", () => {
    // `nextRunAt` 为 null 的两种成因（正在跑 / 还没跑过第一轮）对用户是同一个答案。
    const view = describeNextPull(schedule(null), NOW);
    expect(view.label).toBe("正在拉取");
  });

  it("未来的排程给出时刻与还有多久", () => {
    const view = describeNextPull(schedule(NOW + 720), NOW);
    expect(view.label).toContain("12 分钟");
  });

  it("已到点的排程说即将开始，而不是显示一个过去的时刻", () => {
    const view = describeNextPull(schedule(NOW - 1_000), NOW);
    expect(view.label).toBe("即将开始");
  });

  it("detail 说明轮询间隔，因为真实周期等于间隔加单轮耗时", () => {
    // 后端在一轮**跑完之后**才排下一次，所以「下次」不是「上次 + 间隔」。
    // 不把这层说破，用户会拿这个时间去核对一个对不上的预期。
    const view = describeNextPull(schedule(NOW + 720), NOW);
    expect(view.detail).toContain("15 分钟");
  });

  it("间隔不足一分钟时按秒说，不四舍五入成 0 分钟", () => {
    const view = describeNextPull(schedule(null, 30), NOW);
    expect(view.detail).toContain("30 秒");
  });
});

describe("pullLanded", () => {
  it("lastPullAt 没变就是还没跑到", () => {
    expect(pullLanded(1758000000, item({ lastPullAt: 1758000000 }))).toBe(
      false,
    );
  });

  it("lastPullAt 变了就算落定——失败也算", () => {
    // 后端每轮**开跑就写** last_pull_at，成功与否都写。手动触发要回答的是
    // 「我点的那一轮跑了没有」，不是「它成功了吗」；失败由 consecutiveFailures
    // 与 lastError 呈现。
    expect(
      pullLanded(
        1758000000,
        item({
          lastPullAt: 1758001000,
          consecutiveFailures: 1,
          lastError: "拉取失败",
        }),
      ),
    ).toBe(true);
  });

  it("数据源还没取到时不算落定", () => {
    // 列表还在加载、或那一条被删了就查不到——此时不能当成「跑完了」，
    // 否则按钮会在轮询的第一个节拍就宣布成功。
    expect(pullLanded(null, null)).toBe(false);
  });

  it("从「从未拉取过」到拉过一次也算落定", () => {
    expect(pullLanded(null, item({ lastPullAt: 1758000000 }))).toBe(true);
  });
});

describe("approvalOptionsUrl", () => {
  it("用调用方给的 origin 拼，不硬编码主机名", () => {
    expect(
      approvalOptionsUrl("http://47.109.148.207:18654", "payment_currency"),
    ).toBe(
      "http://47.109.148.207:18654/api/v1/feishu/approval/options/payment_currency",
    );
  });

  it("origin 末尾带斜杠时不产生双斜杠", () => {
    // `window.location.origin` 不带尾斜杠，但这个函数是公开的纯函数——
    // 传进来的人不该因为多了一个斜杠就拿到一个打不开的地址。
    expect(approvalOptionsUrl("http://localhost:5273/", "a")).toBe(
      "http://localhost:5273/api/v1/feishu/approval/options/a",
    );
  });
});
