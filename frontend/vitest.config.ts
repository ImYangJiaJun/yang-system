import { fileURLToPath, URL } from "node:url";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      src: fileURLToPath(new URL("./src", import.meta.url)),
      components: fileURLToPath(new URL("./src/components", import.meta.url)),
      layouts: fileURLToPath(new URL("./src/layouts", import.meta.url)),
      pages: fileURLToPath(new URL("./src/pages", import.meta.url)),
      stores: fileURLToPath(new URL("./src/stores", import.meta.url)),
    },
  },
  test: {
    environment: "jsdom",
    clearMocks: true,
    exclude: ["e2e/**", "node_modules/**", "dist/**", ".quasar/**"],
  },
});
