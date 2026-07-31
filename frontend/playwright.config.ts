import { defineConfig, devices } from "@playwright/test";

const frontendPort = process.env.YANG_E2E_FRONTEND_PORT || "5173";
const backendPort = process.env.YANG_E2E_BACKEND_PORT || "18080";
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
    baseURL: `http://127.0.0.1:${frontendPort}`,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: [
    {
      command: "cargo run --example frontend_demo",
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
      url: `http://127.0.0.1:${frontendPort}`,
      timeout: 120_000,
      reuseExistingServer,
      env: {
        VITE_DEV_PORT: frontendPort,
        VITE_PROXY_TARGET: `http://127.0.0.1:${backendPort}`,
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
