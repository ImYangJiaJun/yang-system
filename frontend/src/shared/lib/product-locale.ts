/**
 * 首发产品唯一支持的语言与地区格式。
 *
 * 该常量不是“默认回退语言”：当前产品合同明确不提供运行时语言切换。新增第二语言前，
 * 必须先完成 frontend/docs/LOCALE.md 中的重新开门条件。
 */
export const PRODUCT_LOCALE = "zh-CN" as const;

export function productLowerCase(value: string): string {
  return value.toLocaleLowerCase(PRODUCT_LOCALE);
}

export function compareProductText(left: string, right: string): number {
  return left.localeCompare(right, PRODUCT_LOCALE);
}

const PRODUCT_DATE_FORMAT = new Intl.DateTimeFormat(PRODUCT_LOCALE);
const PRODUCT_DATE_TIME_FORMAT = new Intl.DateTimeFormat(PRODUCT_LOCALE, {
  dateStyle: "medium",
  timeStyle: "short",
});

/// 以产品唯一 locale 格式化日期；参数是毫秒时间戳或 Date。
export function formatProductDate(value: Date | number): string {
  return PRODUCT_DATE_FORMAT.format(value);
}

/// 以产品唯一 locale 格式化日期+时间；参数是毫秒时间戳或 Date。
export function formatProductDateTime(value: Date | number): string {
  return PRODUCT_DATE_TIME_FORMAT.format(value);
}
