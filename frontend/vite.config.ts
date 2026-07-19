import { fileURLToPath, URL } from "node:url";
import vue from "@vitejs/plugin-vue";
import AutoImport from "unplugin-auto-import/vite";
import Components from "unplugin-vue-components/vite";
import { ElementPlusResolver } from "unplugin-vue-components/resolvers";
import { loadEnv } from "vite";
import { defineConfig } from "vitest/config";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const proxyTarget = env.VITE_PROXY_TARGET || "http://127.0.0.1:8080";

  return {
    plugins: [
      vue(),
      AutoImport({
        resolvers: [ElementPlusResolver()],
        dts: "src/auto-imports.d.ts",
      }),
      Components({
        resolvers: [ElementPlusResolver()],
        dts: "src/components.d.ts",
      }),
    ],
    resolve: {
      alias: {
        "@": fileURLToPath(new URL("./src", import.meta.url)),
      },
    },
    server: {
      port: 5173,
      strictPort: true,
      proxy: {
        "/api": proxyTarget,
        "/.well-known": proxyTarget,
        "/health": proxyTarget,
      },
    },
    test: {
      environment: "jsdom",
      clearMocks: true,
      exclude: ["e2e/**", "node_modules/**", "dist/**"],
    },
  };
});
