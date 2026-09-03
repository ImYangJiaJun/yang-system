import path from "node:path";
import { fileURLToPath } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv } from "vite";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig(({ mode }) => {
  // 开发代理目标：默认对接真实后端；对接无数据库演示后端时
  // 用 VITE_DEV_API_ORIGIN=http://127.0.0.1:18080 启动。
  const env = loadEnv(mode, __dirname, "");
  const devApiOrigin = env.VITE_DEV_API_ORIGIN || "http://127.0.0.1:8080";
  // 注意：不要开 changeOrigin。后端 BrowserSession 同源校验比对 Origin 与 Host，
  // 保持默认（透传 Host）才能让 localhost:5273 的 Origin 与 Host 一致。
  const proxy = {
    "/api": { target: devApiOrigin },
    "/.well-known": { target: devApiOrigin },
  };
  return {
    plugins: [react(), tailwindcss()],
    resolve: {
      alias: {
        "@": path.resolve(__dirname, "src"),
      },
    },
    server: {
      // 避开现有 Quasar 前端的 5173 端口
      port: 5273,
      proxy,
    },
    build: {
      // ADR-1 浏览器契约：chrome111 / firefox128 / safari16.4 的公共子集对应 es2022
      target: "es2022",
    },
  };
});
