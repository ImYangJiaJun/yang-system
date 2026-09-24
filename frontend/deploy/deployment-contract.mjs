export const contentSecurityPolicy = [
  "default-src 'self'",
  "base-uri 'none'",
  "object-src 'none'",
  "form-action 'self'",
  "frame-ancestors 'none'",
  "script-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob:",
  "font-src 'self' data:",
  "connect-src 'self'",
  "worker-src 'self' blob:",
  "manifest-src 'self'",
].join("; ");

/**
 * `<meta>` **交付不了**的 CSP 指令：浏览器只认响应头，写进 `<meta>` 会被直接忽略，
 * 并在控制台报一条错误（2026-09-24 线上实测到 `frame-ancestors` 那一条）。
 *
 * 构建期的 meta 版本必须把它们排除掉；`scripts/verify-production-build.mjs` 按这份
 * 清单核对「meta = 响应头 − 这里」，免得有人把注定失效的指令加回去、或者反过来
 * 让两份清单悄悄漂移。
 */
export const metaIgnoredCspDirectives = Object.freeze([
  "frame-ancestors",
  "report-uri",
  "sandbox",
]);

export const deploymentHeaders = Object.freeze({
  "Content-Security-Policy": contentSecurityPolicy,
  "Cross-Origin-Opener-Policy": "same-origin",
  "Permissions-Policy": "camera=(), geolocation=(), microphone=()",
  "Referrer-Policy": "no-referrer",
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
});

export const cacheControl = Object.freeze({
  html: "no-store",
  immutableAsset: "public, max-age=31536000, immutable",
});
