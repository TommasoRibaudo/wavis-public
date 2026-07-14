// Architecture boundary check for the Rust backend.
//
// CLAUDE.md layering rule: Handler -> Domain -> State, never mixed.
// wavis-backend/src/ws/ is the transport layer: it parses, routes, and
// responds. Auth decisions and database access belong in the auth/domain
// layers and must be *called through* them, not inlined in dispatch.
//
// Baseline policy: existing violations are allowlisted below with an exact
// occurrence count. New occurrences (or new files) fail CI. When you remove
// a baselined call, lower its count here in the same commit.
//
// Run: node scripts/arch-check.mjs   (exit 0 = clean, 1 = violations)

import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

// Resolve rule paths from the repo root so the script works from any CWD.
const REPO_ROOT = dirname(dirname(fileURLToPath(import.meta.url)));

const RULES = [
  {
    name: 'ws-transport-only',
    dir: 'wavis-backend/src/ws',
    forbidden: [
      {
        pattern: /sqlx::/g,
        message: 'direct database access in the transport layer — move the query into a domain/auth function',
      },
      {
        pattern: /jwt::validate_[a-z_]*\(/g,
        message: 'inline JWT validation in the transport layer — call an auth-layer service instead',
      },
      {
        pattern: /auth::check_session_epoch\(/g,
        message: 'inline session-epoch check in the transport layer — belongs behind an auth-layer entry point',
      },
    ],
  },
];

// file (repo-relative, forward slashes) -> pattern source -> allowed count.
// Every entry needs a reason and a removal intent. Currently empty — the
// ws_dispatch.rs auth-arm leaks were extracted into auth::authenticate_access_token.
const BASELINE = {};

function rustFilesUnder(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...rustFilesUnder(full));
    else if (entry.name.endsWith('.rs')) out.push(full);
  }
  return out;
}

let failures = 0;

for (const rule of RULES) {
  for (const file of rustFilesUnder(join(REPO_ROOT, rule.dir))) {
    const relative = file.slice(REPO_ROOT.length + 1).replaceAll('\\', '/');
    const text = readFileSync(file, 'utf8');
    for (const { pattern, message } of rule.forbidden) {
      const count = (text.match(pattern) ?? []).length;
      if (count === 0) continue;
      const allowed = BASELINE[relative]?.[pattern.source] ?? 0;
      if (count > allowed) {
        console.error(
          `[arch-check] ${relative}: ${count} match(es) of /${pattern.source}/ (baseline ${allowed}) — ${message}`,
        );
        failures += 1;
      } else if (count < allowed) {
        console.log(
          `[arch-check] ${relative}: /${pattern.source}/ dropped to ${count} (baseline ${allowed}) — lower the baseline in scripts/arch-check.mjs`,
        );
      }
    }
  }
}

if (failures > 0) {
  console.error(`[arch-check] ${failures} architecture violation(s). See CLAUDE.md layering rules.`);
  process.exit(1);
}

console.log('[arch-check] OK');
