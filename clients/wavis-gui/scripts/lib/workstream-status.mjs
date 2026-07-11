import fs from 'node:fs';
import path from 'node:path';

const STATUS_FILE_REL = '.kiro/specs/audio-video-polish/workstream-status.json';

export function resolveStatusPath(repoRoot) {
  return path.resolve(repoRoot, STATUS_FILE_REL);
}

export function readWorkstreamStatus(repoRoot) {
  const filePath = resolveStatusPath(repoRoot);
  if (!fs.existsSync(filePath)) {
    throw new Error(`workstream-status.json not found at ${filePath}`);
  }
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

/**
 * Throws with a clear message if any of the required workstreams are not
 * exit_criteria_met in the status file. Consumed by the W4 shootout runner
 * and the W5 auto-switch gate. See design.md §8 gate rules.
 */
export function assertEntryGate(status, requiredWorkstreams) {
  const failing = requiredWorkstreams.filter((w) => !status[w]?.exit_criteria_met);
  if (failing.length > 0) {
    const details = failing
      .map(
        (w) =>
          `  ${w}: exit_criteria_met=${status[w]?.exit_criteria_met ?? 'MISSING'}, notes="${status[w]?.notes ?? ''}"`,
      )
      .join('\n');
    throw new Error(
      `[workstream-status] Entry gate blocked — the following workstreams have not met exit criteria:\n${details}\n\nSet exit_criteria_met=true in ${STATUS_FILE_REL} after manual verification.`,
    );
  }
}

export function readDecision(status, workstream) {
  return status[workstream]?.decision ?? null;
}

export function writeWorkstreamStatus(repoRoot, status) {
  const filePath = resolveStatusPath(repoRoot);
  fs.writeFileSync(filePath, JSON.stringify(status, null, 2) + '\n', 'utf8');
}
