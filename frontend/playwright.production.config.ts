import { defineConfig, devices } from "@playwright/test";

const frontendPort = process.env.YANG_PRODUCTION_E2E_FRONTEND_PORT || "5300";
const backendPort = process.env.YANG_PRODUCTION_E2E_BACKEND_PORT || "18300";

export default defineConfig({
  testDir: "./e2e-production",
  outputDir: "test-results-production",
  fullyParallel: false,
  forbidOnly: true,
  retries: 0,
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
      reuseExistingServer: false,
      env: {
        YANG_DEMO_BIND: `127.0.0.1:${backendPort}`,
      },
    },
    {
      command: "pnpm build && node scripts/serve-production-build.mjs",
      url: `http://127.0.0.1:${frontendPort}`,
      timeout: 180_000,
      reuseExistingServer: false,
      env: {
        YANG_PRODUCTION_E2E_FRONTEND_PORT: frontendPort,
        YANG_PRODUCTION_E2E_BACKEND_PORT: backendPort,
      },
    },
  ],
  projects: [
    {
      name: "production-chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
