import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
      "@test": path.resolve(__dirname, "tests"),
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./tests/setup.ts"],
    // e2e/ 与 e2e-production/ 是 Playwright 规格，不走 Vitest。
    // 单元测试与 src/ 生产代码隔离，集中在 tests/ 并镜像 src 目录结构。
    include: ["tests/**/*.{test,spec}.{ts,tsx}"],
  },
});
