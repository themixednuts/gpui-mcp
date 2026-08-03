import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'parity.spec.mjs',
  outputDir: 'artifacts/test-output',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['line']],
  timeout: 120_000,
  expect: {
    timeout: 10_000,
  },
  use: {
    browserName: 'chromium',
    channel: 'chromium',
    headless: true,
  },
});
