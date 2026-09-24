import path from "node:path";
import { fileURLToPath } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv, type Plugin } from "vite";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/// 生产 CSP 的 meta 版本。
///
/// 口径是「**响应头清单减去 `<meta>` 交付不了的指令**」，响应头那份的权威定义在
/// `deploy/deployment-contract.mjs`（`contentSecurityPolicy`）。这里目前唯一的差项是
/// `frame-ancestors`：
///
/// 规范规定 `frame-ancestors` 只认 HTTP 响应头，放进 `<meta>` 会被浏览器**直接忽略**
/// 并在控制台打一条错误（`The Content Security Policy directive 'frame-ancestors' is
/// ignored when delivered via a <meta> element.`）。写在这里既不生效，又是一条常驻噪音
/// ——2026-09-24 在线上控制台实测到了它。防嵌入由 nginx 的响应头承担，那条有
/// `frontend/e2e-production/production-build.spec.ts` 盯着。
///
/// 同名清单不再靠注释维系：`scripts/verify-production-build.mjs` 按
/// `deploy/deployment-contract.mjs` 的 `metaIgnoredCspDirectives` **推导**出 meta 应有
/// 的指令集合，缺一条、或者把必然失效的那几条加回来，构建门禁都会红。
const CONTENT_SECURITY_POLICY = [
  "default-src 'self'",
  "base-uri 'none'",
  "object-src 'none'",
  "form-action 'self'",
  "script-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob:",
  "font-src 'self' data:",
  "connect-src 'self'",
  "worker-src 'self' blob:",
  "manifest-src 'self'",
].join("; ");

/// CSP meta 只在生产构建注入：Vite dev 的 react-refresh preamble 是内联脚本，
/// 与 script-src 'self' 冲突；生产产物全是外联 chunk。
function cspMetaPlugin(): Plugin {
  return {
    name: "yang-csp-meta",
    apply: "build",
    transformIndexHtml(html) {
      return {
        html,
        tags: [
          {
            tag: "meta",
            attrs: {
              "http-equiv": "Content-Security-Policy",
              content: CONTENT_SECURITY_POLICY,
            },
            injectTo: "head-prepend",
          },
        ],
      };
    },
  };
}

export default defineConfig(({ mode }) => {
  // 开发代理目标：默认对接真实后端；对接无数据库演示后端时
  // 用 VITE_DEV_API_ORIGIN=http://127.0.0.1:18080 启动。
  // E2E 用 VITE_DEV_PORT 覆盖端口（dev-server 环境 5310）。
  const env = loadEnv(mode, __dirname, "");
  const devApiOrigin = env.VITE_DEV_API_ORIGIN || "http://127.0.0.1:8080";
  const devPort = Number(env.VITE_DEV_PORT) || 5273;
  // 注意：不要开 changeOrigin。后端 BrowserSession 同源校验比对 Origin 与 Host，
  // 保持默认（透传 Host）才能让 localhost:5273 的 Origin 与 Host 一致。
  const proxy = {
    "/api": { target: devApiOrigin },
    "/.well-known": { target: devApiOrigin },
  };
  return {
    plugins: [react(), tailwindcss(), cspMetaPlugin()],
    resolve: {
      alias: {
        "@": path.resolve(__dirname, "src"),
      },
    },
    server: {
      port: devPort,
      proxy,
    },
    build: {
      // ADR-1 浏览器契约：chrome111 / firefox128 / safari16.4 的公共子集对应 es2022
      target: "es2022",
      // 供 scripts/verify-bundle-budget.mjs 计算首屏口径（入口 + 静态依赖图）。
      manifest: true,
      // 首屏预算（ADR-5 §2.4）：路由级 lazy（workbench/custom view）之外，
      // 把框架与底层库切成独立可缓存 chunk，压低入口 chunk 体积。
      rollupOptions: {
        output: {
          advancedChunks: {
            groups: [
              { name: "react", test: /node_modules[\\/]react/ },
              { name: "react", test: /node_modules[\\/](scheduler)/ },
              {
                name: "router",
                test: /node_modules[\\/](react-router|@remix-run)/,
              },
              { name: "tanstack", test: /node_modules[\\/]@tanstack/ },
              { name: "radix", test: /node_modules[\\/]@radix-ui/ },
            ],
          },
        },
      },
    },
  };
});
