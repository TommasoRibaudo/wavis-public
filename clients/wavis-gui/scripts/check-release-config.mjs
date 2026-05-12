/**
 * Pre-release configuration guard.
 *
 * 1. Fails if any forbidden production override env vars are set.
 * 2. Fails if W1 exit criteria are not met in workstream-status.json (design.md §2.2, R11.1).
 *
 * Run via: node scripts/check-release-config.mjs <repo-root>
 * Wired into CI release pipeline.
 */

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { readWorkstreamStatus, assertEntryGate } from './lib/workstream-status.mjs';

// ─── Forbidden production overrides ──────────────────────────────────────────

const forbiddenKeys = ['VITE_WAVIS_WINDOWS_SHARE_PATH'];

const presentKeys = forbiddenKeys.filter((key) =>
  Object.prototype.hasOwnProperty.call(process.env, key),
);

if (presentKeys.length > 0) {
  console.error(
    `[release-config] forbidden production override(s) detected: ${presentKeys.join(', ')}`,
  );
  process.exit(1);
}

// ─── Workstream exit-criteria gate (§2.2, R11.1) ─────────────────────────────

// Resolve repo root: either from CLI arg or relative to this file.
const __dirname = path.dirname(fileURLToPath(import.meta.url));
// scripts/ → wavis-gui/ → clients/ → repo root
const repoRoot = process.argv[2] ?? path.resolve(__dirname, '../../..');

let status;
try {
  status = readWorkstreamStatus(repoRoot);
} catch (err) {
  console.error(`[release-config] could not read workstream-status.json: ${err.message}`);
  process.exit(1);
}

// W1 must be exit_criteria_met before any release ships (transceiver/RSS bounds §2.2).
// W3 must be exit_criteria_met (Linux capability matrix verified, R10.1).
// W2, W4, W5, W6 are checked separately by their own gates and are allowed to be
// in-progress at release time (W2 integration test is hardware-gated; W4-W6 are
// incremental improvements that ship behind gates).
try {
  assertEntryGate(status, ['W1', 'W3']);
} catch (err) {
  console.error(`[release-config] ${err.message}`);
  process.exit(1);
}

console.log('[release-config] OK');
