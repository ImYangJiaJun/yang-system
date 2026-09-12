/**
 * 头像上传前的客户端图片处理：等比缩放 + WebP 质量迭代。
 * 决策逻辑（尺寸换算 / 字节数 / 质量阶梯）与 canvas 操作分离，
 * 纯函数部分可在 jsdom（无真实 canvas）下单测。
 */

/// 后端约束：base64 解码后 ≤ 40KiB。
export const AVATAR_MAX_BYTES = 40 * 1024;
/// 客户端缩放目标：等比缩放到宽高均不超过该值（不放大）。
export const AVATAR_MAX_DIMENSION = 256;
export const AVATAR_MIME = "image/webp";
export const AVATAR_QUALITY_START = 0.9;
export const AVATAR_QUALITY_MIN = 0.5;
export const AVATAR_QUALITY_STEP = 0.1;

/// 等比缩放：宽高均不超过 maxDimension，小图不放大，结果至少 1px。
export function fitWithinMaxDimension(
  width: number,
  height: number,
  maxDimension: number = AVATAR_MAX_DIMENSION,
): { width: number; height: number } {
  if (width <= 0 || height <= 0) {
    throw new Error("图片尺寸无效");
  }
  const scale = Math.min(1, maxDimension / Math.max(width, height));
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale)),
  };
}

/// base64 串解码后的字节数（按结尾 padding 精确换算）。
export function base64DecodedBytes(base64: string): number {
  const padding = base64.endsWith("==") ? 2 : base64.endsWith("=") ? 1 : 0;
  return Math.floor((base64.length * 3) / 4) - padding;
}

export type QualityDecision =
  | { kind: "accept"; quality: number }
  | { kind: "retry"; quality: number }
  | { kind: "too-large" };

/**
 * 质量迭代决策：给定已尝试的 (quality → 解码字节数)，返回下一步——
 * 最近一次已达标则接受；否则按阶梯降质重试；跌破质量下限仍超标则放弃。
 */
export function decideQuality(
  attempts: ReadonlyArray<{ quality: number; bytes: number }>,
  maxBytes: number = AVATAR_MAX_BYTES,
): QualityDecision {
  const last = attempts[attempts.length - 1];
  if (!last) return { kind: "retry", quality: AVATAR_QUALITY_START };
  if (last.bytes <= maxBytes) return { kind: "accept", quality: last.quality };
  const next = Math.round((last.quality - AVATAR_QUALITY_STEP) * 10) / 10;
  if (next < AVATAR_QUALITY_MIN) return { kind: "too-large" };
  return { kind: "retry", quality: next };
}

export type PreparedAvatar = {
  contentBase64: string;
  mime: string;
};

/// 降质迭代到下限仍超限时抛出，页面层展示该消息。
export class AvatarTooLargeError extends Error {
  constructor() {
    super("图片过大，请更换");
    this.name = "AvatarTooLargeError";
  }
}

/**
 * canvas 路径：读文件 → 等比缩放到 ≤256×256 → 导出 WebP，
 * 从 quality 0.9 起逐步降质直到 base64 解码后 ≤40KiB。
 */
export async function prepareAvatarFile(
  file: File | Blob,
): Promise<PreparedAvatar> {
  const bitmap = await createImageBitmap(file);
  try {
    const { width, height } = fitWithinMaxDimension(
      bitmap.width,
      bitmap.height,
    );
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext("2d");
    if (!context) {
      throw new Error("当前浏览器不支持图片处理");
    }
    context.drawImage(bitmap, 0, 0, width, height);

    const attempts: { quality: number; bytes: number }[] = [];
    let quality = AVATAR_QUALITY_START;
    for (;;) {
      const dataUrl = canvas.toDataURL(AVATAR_MIME, quality);
      const contentBase64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
      attempts.push({ quality, bytes: base64DecodedBytes(contentBase64) });
      const decision = decideQuality(attempts);
      if (decision.kind === "accept") {
        return { contentBase64, mime: AVATAR_MIME };
      }
      if (decision.kind === "too-large") {
        throw new AvatarTooLargeError();
      }
      quality = decision.quality;
    }
  } finally {
    bitmap.close();
  }
}
