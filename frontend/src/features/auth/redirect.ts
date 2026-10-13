/// 登录回跳目标的开放重定向防护：只放行同源相对路径。
/// 排除 `//host`（协议相对）、`/\host`（部分浏览器把 `\` 归一成 `/`）、
/// 绝对 URL（`http://host`），以及空白/控制字符——浏览器 URL 解析器会先剔除
/// 制表符与换行，`/\t/evil.com` 剥完就变成 `//evil.com`。
/// 缺失、为 `/` 本身或任何非法形态一律落到 `/`。
/// 首字符必须是 `/`，第二个字符不能是 `/` 或 `\`（协议相对与反斜杠归一），
/// 其后允许 `/`（正常多级路径），但不允许反斜杠与空白/控制字符。
const SAFE_REDIRECT_PATTERN = /^\/[^/\\\s][^\\\s]*$/;

export function safeRedirectTarget(raw: string | null): string {
  return raw !== null && SAFE_REDIRECT_PATTERN.test(raw) ? raw : "/";
}
