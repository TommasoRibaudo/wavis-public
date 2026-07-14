// Playwright Test config for the dev-only Wavis GUI harness. Tests never
// use Playwright's own browser/context/page fixtures — the `app` fixture in
// tests/fixtures.mjs attaches to the real Tauri app via driver.mjs's CDP
// connection instead. See README.md for the full CDP-vs-WebDriver story.
import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  // Prebuilds the ws-sfu-test peer binary so spawnPeer()'s `cargo run` never
  // compiles mid-spec (a cold compile outlasts the peer's 15s auth timeout).
  globalSetup: './global-setup.mjs',
  // The app launch is stateful and pinned to a single fixed CDP port
  // (driver.mjs defaults to 9222) — parallel workers would collide launching
  // separate app instances against the same port.
  workers: 1,
  fullyParallel: false,
  retries: 0,
  reporter: 'list',
  timeout: 30_000,
});
