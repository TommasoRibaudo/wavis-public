/**
 * Record the W4 codec-shootout decision and auto-flip dependent gates.
 *
 * Usage:
 *   node scripts/record-w4-decision.mjs <decision> [<repo-root>]
 *
 * <decision> must be one of: vp9vp8 | vp8sim | av1vp9
 *
 * What this does:
 *   1. Stamps W4.decision and sets W4.exit_criteria_met = true.
 *   2. Auto-flips W5.exit_criteria_met = true — W5's only pending condition
 *      was "W4/decision recorded"; the motion-detector auto-switch code is
 *      already unconditional (A1.5) so no further code gate exists.
 *   3. Writes the updated workstream-status.json.
 *
 * Design: §4.4 outcome handling, §A4.3.
 */

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { readWorkstreamStatus, writeWorkstreamStatus } from './lib/workstream-status.mjs';

const VALID_DECISIONS = ['vp9vp8', 'vp8sim', 'av1vp9'];

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// scripts/ → wavis-gui/ → clients/ → repo root
const DEFAULT_REPO_ROOT = path.resolve(__dirname, '../../..');

const [decision, repoRoot = DEFAULT_REPO_ROOT] = process.argv.slice(2);

if (!decision) {
  console.error(`Usage: node scripts/record-w4-decision.mjs <decision> [<repo-root>]`);
  console.error(`  decision: ${VALID_DECISIONS.join(' | ')}`);
  process.exit(1);
}

if (!VALID_DECISIONS.includes(decision)) {
  console.error(
    `[record-w4-decision] unknown decision "${decision}". Must be one of: ${VALID_DECISIONS.join(', ')}`,
  );
  process.exit(1);
}

const status = readWorkstreamStatus(repoRoot);
const now = new Date().toISOString();

status.W4 = {
  ...status.W4,
  exit_criteria_met: true,
  verified_at: now,
  decision,
  notes: status.W4?.notes ?? '',
};

status.W5 = {
  ...status.W5,
  exit_criteria_met: true,
  verified_at: now,
  notes: `Auto-flipped when W4/decision="${decision}" was recorded. Motion-detector auto-switch is unconditional (A1.5); no further code gate exists.`,
};

writeWorkstreamStatus(repoRoot, status);

console.log(`[record-w4-decision] W4.decision = "${decision}"  →  exit_criteria_met = true`);
console.log(`[record-w4-decision] W5 auto-flipped              →  exit_criteria_met = true`);
console.log(`[record-w4-decision] Written to workstream-status.json`);
