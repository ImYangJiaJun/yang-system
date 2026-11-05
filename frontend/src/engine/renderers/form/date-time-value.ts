/**
 * `date_time` 控件与后端的双向格式桥。
 *
 * 展示侧把时间戳按 **Unix 秒** 解读（business-cell-model.ts），表单里却是
 * `<input type="datetime-local">`：它只认 `YYYY-MM-DDTHH:mm`（本地墙钟，无时区）。
 * 直接把行里的 `1720000000` 塞进 value，浏览器会拒绝渲染 —— 编辑框空白，
 * 用户重选后提交的又是字符串，与后端 `Timestamp` 不符。
 *
 * 放在独立模块而不是 SchemaField.tsx：组件文件只导出组件，
 * 否则 react-refresh 会警告（规则见 eslint react-refresh/only-export-components）。
 */

///
/// 展示侧把时间戳按 **Unix 秒** 解读（business-cell-model.ts），表单里却是
/// `<input type="datetime-local">`：它只认 `YYYY-MM-DDTHH:mm`（本地墙钟，无时区）。
/// 直接把行里的 `1720000000` 塞进 value，浏览器会拒绝渲染 —— 编辑框空白，
/// 用户重选后提交的又是字符串，与后端 `Timestamp` 不符。这里显式换算：
/// - 入：秒 / 秒字符串 / ISO 字符串 → 本地墙钟字符串；
/// - 出：本地墙钟字符串 → Unix 秒（提交口径与展示口径一致）。
export function toDateTimeLocalValue(value: unknown): string {
  if (
    typeof value === "string" &&
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(value)
  )
    return value;
  const date = toFormDate(value);
  if (!date) return "";
  const pad = (input: number) => String(input).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(
    date.getDate(),
  )}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function fromDateTimeLocalValue(value: string): unknown {
  if (!value) return undefined;
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? undefined
    : Math.floor(date.getTime() / 1000);
}

function toFormDate(value: unknown): Date | undefined {
  if (typeof value === "number" && Number.isFinite(value))
    return new Date(value * 1000);
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (!trimmed) return undefined;
    if (/^-?\d+(?:\.\d+)?$/.test(trimmed))
      return new Date(Number(trimmed) * 1000);
    const parsed = new Date(trimmed);
    return Number.isNaN(parsed.getTime()) ? undefined : parsed;
  }
  return undefined;
}
