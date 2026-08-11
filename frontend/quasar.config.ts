import { defineConfig } from "#q-app/wrappers";

export default defineConfig(() => ({
  boot: ["observability", "theme"],
  css: ["app.css"],
  extras: ["material-icons"],
  build: {
    target: {
      browser: ["es2022", "firefox115", "chrome115", "safari14"],
      node: "node20",
    },
    vueRouterMode: "history",
  },
  devServer: {
    host: "127.0.0.1",
    port: Number(process.env.VITE_DEV_PORT || "5173"),
    open: false,
    proxy: {
      "/api": {
        target: process.env.VITE_PROXY_TARGET || "http://127.0.0.1:8080",
      },
      "/.well-known": {
        target: process.env.VITE_PROXY_TARGET || "http://127.0.0.1:8080",
      },
      "/health": {
        target: process.env.VITE_PROXY_TARGET || "http://127.0.0.1:8080",
      },
    },
  },
  framework: {
    config: {
      brand: {
        primary: "#0f766e",
        secondary: "#14b8a6",
        accent: "#0f766e",
        positive: "#3f9667",
        negative: "#b7312c",
        warning: "#bd862f",
      },
      notify: { position: "top-right", timeout: 2500 },
    },
    lang: "zh-CN",
    plugins: ["Dark", "Dialog", "Notify"],
  },
}));
