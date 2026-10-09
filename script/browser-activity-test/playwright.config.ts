import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  workers: 1,
  fullyParallel: false,
  outputDir: './artifacts/results',
  use: {
    baseURL: 'http://127.0.0.1:5173',
    channel: process.env.ACTIVITY_TEST_BROWSER_CHANNEL,
    viewport: { width: 1280, height: 900 },
    trace: 'retain-on-failure',
  },
  webServer: {
    command: 'npm run dev',
    url: 'http://127.0.0.1:5173',
    reuseExistingServer: !process.env.CI,
  },
});
