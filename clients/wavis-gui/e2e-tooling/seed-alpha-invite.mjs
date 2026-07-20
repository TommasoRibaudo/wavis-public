// Idempotently (re)inserts a multi-use closed-alpha invite into the local
// postgres so live-backend-helpers.mjs's registerDevice() never dies with
// "ALPHA_INVITE_CODE must be set". Pulls ALPHA_INVITE_CODE_PEPPER from the
// running wavis-backend container rather than hardcoding it, so this stays
// correct even if the pepper is ever overridden.
//
// Runs via execFileSync (no shell), so this is immune to the Windows
// VS Code task quoting bug where an inline `node -e "..."` inside a
// tasks.json shell command gets its nested double-quotes eaten by the
// extra powershell.exe -Command "..." wrapper VS Code adds.
import { execFileSync } from 'node:child_process';
import { createHmac } from 'node:crypto';

const CODE = process.argv[2] || 'E2EALPHA174';

const pepper = execFileSync(
  'docker',
  ['exec', 'wavis-backend', 'printenv', 'ALPHA_INVITE_CODE_PEPPER'],
  { encoding: 'utf8' },
).trim();

const hex = createHmac('sha256', pepper).update(`alpha-invite:v1:${CODE}`).digest('hex');

const sql = `INSERT INTO alpha_invites (code_hash, max_redemptions) VALUES (decode('${hex}','hex'), 100000) ON CONFLICT (code_hash) DO UPDATE SET max_redemptions=100000, redemption_count=0, disabled_at=NULL, expires_at=NULL;`;

execFileSync('docker', ['exec', 'wavis-postgres', 'psql', '-U', 'wavis', '-d', 'wavis', '-c', sql], {
  stdio: 'inherit',
});

console.log(`Seeded alpha invite "${CODE}"`);
