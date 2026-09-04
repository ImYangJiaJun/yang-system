import { defineConfig, devices } from "@playwright/test";

const frontendPort = process.env.YANG_E2E_FRONTEND_PORT || "5310";
const backendPort = process.env.YANG_E2E_BACKEND_PORT || "18310";
const reuseExistingServer =
  process.env.YANG_E2E_REUSE_EXISTING_SERVER === "true";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: "list",
  use: {
    baseURL: `http://localhost:${frontendPort}`,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: [
    {
      // 演示后端绑定隔离端口（YANG_DEMO_BIND 契约见 examples/frontend_demo）。
      command: "cargo run --locked --example frontend_demo",
      cwd: "..",
      url: `http://127.0.0.1:${backendPort}/health/live`,
      timeout: 180_000,
      reuseExistingServer,
      env: {
        YANG_DEMO_BIND: `127.0.0.1:${backendPort}`,
      },
    },
    {
      command: "pnpm dev",
      url: `http://localhost:${frontendPort}`,
      timeout: 120_000,
      reuseExistingServer,
      env: {
        VITE_DEV_PORT: frontendPort,
        VITE_DEV_API_ORIGIN: `http://127.0.0.1:${backendPort}`,
      },
    },
  ],
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
    // Firefox/WebKit 暂不启用：演示后端是跨 project 共享的内存态单例，
    // 表格类用例会真实增删数据，多浏览器并发会产生顺序耦合（firefox 实测全军覆没）。
    // 与旧前端一致只跑 chromium；浏览器已安装，用例改造为状态无关后可放开。
  ],
});
