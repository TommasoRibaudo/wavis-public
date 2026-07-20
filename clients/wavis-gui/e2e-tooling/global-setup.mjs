// Prebuilds the ws-sfu-test peer binary before any spec runs. spawnPeer()
// (tests/live-backend-helpers.mjs) launches it via `cargo run`, which
// compiles on demand — on a cold target dir that compile silently eats the
// peer's 15s auth-output timeout and fails whichever peer-using spec runs
// first (observed: audio-received, with the fresh exe's mtime landing
// mid-run). Building here moves that wait ahead of the suite instead; on a
// warm target dir this is a sub-second no-op.
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// e2e-tooling -> wavis-gui -> clients -> workspace root
const WORKSPACE_ROOT = path.resolve(__dirname, '..', '..', '..');

execFileSync('cargo', ['build', '-p', 'ws-sfu-test'], {
  cwd: WORKSPACE_ROOT,
  stdio: 'inherit',
});
