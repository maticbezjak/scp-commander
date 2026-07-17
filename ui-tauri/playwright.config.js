import { defineConfig, devices } from "@playwright/test";

// End-to-end tests for the Svelte frontend. They run against the real built
// bundle (`vite preview` serving dist/) in WebKit — the same engine as the
// app's webview (WKWebView on macOS, WebKitGTK on Linux) — with the Tauri
// bridge stubbed (see tests/harness.js). No Rust backend is involved.
export default defineConfig({
  testDir: "./tests",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: "list",
  use: {
    baseURL: "http://localhost:4173",
    trace: process.env.CI ? "retain-on-failure" : "off",
  },
  projects: [{ name: "webkit", use: { ...devices["Desktop Safari"] } }],
  webServer: {
    // Build + serve the production bundle the app actually ships.
    command: "npm run build && npm run preview -- --port 4173 --strictPort",
    port: 4173,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
