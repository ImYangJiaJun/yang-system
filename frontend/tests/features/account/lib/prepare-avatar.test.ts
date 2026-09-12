import { describe, expect, it } from "vitest";

import {
  AVATAR_MAX_BYTES,
  AVATAR_MAX_DIMENSION,
  AVATAR_QUALITY_MIN,
  AVATAR_QUALITY_START,
  base64DecodedBytes,
  decideQuality,
  fitWithinMaxDimension,
} from "@/features/account/lib/prepare-avatar";

/// prepare-avatar 决策逻辑：尺寸换算、base64 字节数、质量阶梯迭代。
/// canvas 操作不在 jsdom 覆盖范围（无真实 canvas 实现）。

describe("fitWithinMaxDimension", () => {
  it("宽图等比缩放到宽度上限", () => {
    expect(fitWithinMaxDimension(1024, 512)).toEqual({
      width: 256,
      height: 128,
    });
  });

  it("高图等比缩放到高度上限", () => {
    expect(fitWithinMaxDimension(200, 800)).toEqual({
      width: 64,
      height: 256,
    });
  });

  it("小图不放大", () => {
    expect(fitWithinMaxDimension(100, 80)).toEqual({ width: 100, height: 80 });
  });

  it("恰好在上限内保持不变", () => {
    expect(
      fitWithinMaxDimension(AVATAR_MAX_DIMENSION, AVATAR_MAX_DIMENSION),
    ).toEqual({ width: 256, height: 256 });
  });

  it("极端宽高比缩放后至少 1px", () => {
    const result = fitWithinMaxDimension(10000, 3);
    expect(result.width).toBe(AVATAR_MAX_DIMENSION);
    expect(result.height).toBe(1);
  });

  it("非法尺寸抛错", () => {
    expect(() => fitWithinMaxDimension(0, 100)).toThrow(/尺寸无效/);
    expect(() => fitWithinMaxDimension(100, -1)).toThrow(/尺寸无效/);
  });
});

describe("base64DecodedBytes", () => {
  it("无 padding", () => {
    // "ABC" → QUJD（4 字符 = 3 字节）
    expect(base64DecodedBytes("QUJD")).toBe(3);
  });

  it("单 padding", () => {
    // "AB" → QUI=（4 字符 - 1 padding = 2 字节）
    expect(base64DecodedBytes("QUI=")).toBe(2);
  });

  it("双 padding", () => {
    // "A" → QQ==（4 字符 - 2 padding = 1 字节）
    expect(base64DecodedBytes("QQ==")).toBe(1);
  });

  it("空串为 0", () => {
    expect(base64DecodedBytes("")).toBe(0);
  });
});

describe("decideQuality", () => {
  it("无尝试记录时从起始质量开始", () => {
    expect(decideQuality([])).toEqual({
      kind: "retry",
      quality: AVATAR_QUALITY_START,
    });
  });

  it("最近一次尝试已达标则接受", () => {
    expect(decideQuality([{ quality: 0.9, bytes: AVATAR_MAX_BYTES }])).toEqual({
      kind: "accept",
      quality: 0.9,
    });
    expect(
      decideQuality([
        { quality: 0.9, bytes: AVATAR_MAX_BYTES + 1 },
        { quality: 0.8, bytes: AVATAR_MAX_BYTES - 1 },
      ]),
    ).toEqual({ kind: "accept", quality: 0.8 });
  });

  it("超标则按阶梯降质重试", () => {
    expect(
      decideQuality([{ quality: 0.9, bytes: AVATAR_MAX_BYTES + 1 }]),
    ).toEqual({ kind: "retry", quality: 0.8 });
  });

  it("降质到下限仍超标则放弃", () => {
    const attempts = [0.9, 0.8, 0.7, 0.6, AVATAR_QUALITY_MIN].map(
      (quality) => ({ quality, bytes: AVATAR_MAX_BYTES + 1 }),
    );
    expect(decideQuality(attempts)).toEqual({ kind: "too-large" });
  });

  it("自定义上限同样生效", () => {
    expect(decideQuality([{ quality: 0.9, bytes: 101 }], 100)).toEqual({
      kind: "retry",
      quality: 0.8,
    });
    expect(decideQuality([{ quality: 0.9, bytes: 100 }], 100)).toEqual({
      kind: "accept",
      quality: 0.9,
    });
  });
});
