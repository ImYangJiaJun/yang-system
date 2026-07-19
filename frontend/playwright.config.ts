import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: "list",
  use: {
    baseURL: "http://127.0.0.1:5173",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: [
    {
      command: "cargo run --bin frontend_demo",
      cwd: "..",
      url: "http://127.0.0.1:18080/health/live",
      timeout: 180_000,
      reuseExistingServer: !process.env.CI,
      env: {
        YANG_DEMO_BIND: "127.0.0.1:18080",
      },
    },
    {
      command: "pnpm dev",
      url: "http://127.0.0.1:5173",
      timeout: 120_000,
      reuseExistingServer: !process.env.CI,
      env: {
        VITE_PROXY_TARGET: "http://127.0.0.1:18080",
      },
    },
  ],
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
