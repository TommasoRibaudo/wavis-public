/**
 * Codec-policy shootout runner — W4 measurement harness.
 *
 * Usage:
 *   node scripts/codec-shootout/run-matrix.mjs \
 *     --app-binary <path-to-wavis-app> \
 *     --livekit-url <wss://...> \
 *     --livekit-api-key <key> \
 *     --livekit-api-secret <secret> \
 *     [--cells vp9vp8-p2-detail-detail,av1vp9-p4-detail-detail]   # subset
 *     [--run-id <id>]                                               # auto-generated if omitted
 *     [--dry-run]                                                   # print plan, do not execute
 *
 * Requirements: R11.1 (entry gate), R11.2 (baseline), R11.3 (measurements),
 *   R11.6 (dynacast:false), R11.7 (no setVideoQuality on VP9 SVC),
 *   R7.3/R17.5 (reuse patch verified).
 * Design: §2.6, §4.4.
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { createHmac } from 'node:crypto';
import { readWorkstreamStatus, assertEntryGate } from '../lib/workstream-status.mjs';
import { verifyPatchMarkers } from '../apply-livekit-transceiver-reuse-fix.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SCRIPTS_DIR = path.resolve(__dirname, '..');
const GUI_DIR = path.resolve(SCRIPTS_DIR, '..');
const REPO_ROOT = path.resolve(GUI_DIR, '../..');

const MATRIX_PATH = path.resolve(__dirname, 'matrix.json');
const LK_MEDIA_SRC = path.resolve(GUI_DIR, 'src/features/voice/livekit-media.ts');
const LK_ESM_PATH = path.resolve(GUI_DIR, 'node_modules/livekit-client/dist/livekit-client.esm.mjs');

const ARTIFACTS_BASE = path.resolve(GUI_DIR, 'artifacts/codec-shootout');
const FIXTURES_DIR = path.resolve(ARTIFACTS_BASE, 'fixtures');

const REQUIRED_ENTRY_WORKSTREAMS = ['W1', 'W2', 'W3'];

/* ─── CLI parsing ────────────────────────────────────────────────── */

function parseArgs(argv) {
  const args = {
    appBinary: null,
    livekitUrl: null,
    livekitApiKey: null,
    livekitApiSecret: null,
    cellFilter: null,
    runId: null,
    dryRun: false,
    skipW2Gate: false,
    skipFixtureCheck: false,
  };
  for (let i = 0; i < argv.length; i++) {
    switch (argv[i]) {
      case '--app-binary':         args.appBinary = argv[++i]; break;
      case '--livekit-url':        args.livekitUrl = argv[++i]; break;
      case '--livekit-api-key':    args.livekitApiKey = argv[++i]; break;
      case '--livekit-api-secret': args.livekitApiSecret = argv[++i]; break;
      case '--cells':              args.cellFilter = new Set(argv[++i].split(',')); break;
      case '--run-id':             args.runId = argv[++i]; break;
      case '--dry-run':            args.dryRun = true; break;
      case '--skip-w2-gate':       args.skipW2Gate = true; break;
      case '--skip-fixture-check': args.skipFixtureCheck = true; break;
    }
  }
  return args;
}

/* ─── Failure helpers ────────────────────────────────────────────── */

function fail(message) {
  console.error(`\n[shootout] ABORT: ${message}\n`);
  process.exit(1);
}

function log(message) {
  console.log(`[shootout] ${message}`);
}

/* ─── Entry-condition gate (R11.1) ──────────────────────────────── */

function checkEntryConditions(skipW2Gate) {
  const required = skipW2Gate
    ? REQUIRED_ENTRY_WORKSTREAMS.filter((w) => w !== 'W2')
    : REQUIRED_ENTRY_WORKSTREAMS;
  log(`Checking workstream entry conditions (${required.join('/')})${skipW2Gate ? ' [W2 skipped]' : ''}...`);
  let status;
  try {
    status = readWorkstreamStatus(REPO_ROOT);
  } catch (err) {
    fail(`Could not read workstream-status.json: ${err.message}`);
  }
  try {
    assertEntryGate(status, required);
  } catch (err) {
    fail(err.message);
  }
  log(`Entry conditions satisfied: ${required.join(', ')} all exit_criteria_met=true`);
}

/* ─── Anti-pattern assertions (R11.6, R11.7, R7.3/R17.5) ────────── */

function assertDynacaseDisabledInMatrix(cells) {
  const violations = cells.filter((c) => c.dynacast !== false);
  if (violations.length > 0) {
    fail(
      `dynacast must be false in every cell (R11.6, R17.2). Violations:\n` +
      violations.map((c) => `  ${c.cellId}: dynacast=${c.dynacast}`).join('\n'),
    );
  }
  log(`dynacast:false verified in all ${cells.length} cells`);
}

function assertNoSetVideoQualityOnVp9(sourceFile) {
  if (!fs.existsSync(sourceFile)) {
    log(`WARN: livekit-media.ts not found at ${sourceFile} — skipping VP9 setVideoQuality scan`);
    return;
  }
  const src = fs.readFileSync(sourceFile, 'utf8');
  // Detect any call to setVideoQuality not guarded by a comment explicitly
  // allowing it on a non-VP9 track. The pattern is: setVideoQuality inside any
  // code path that publishes a VP9 SVC track. Since VP9 SVC is the current
  // primary codec, any unconditional setVideoQuality is a violation.
  // R11.7, R17.1: setVideoQuality is a no-op on VP9 SVC and must not be called.
  const setVideoQualityRegex = /setVideoQuality\s*\(/g;
  const matches = [...src.matchAll(setVideoQualityRegex)];
  if (matches.length > 0) {
    const lines = src.split('\n');
    const hitLines = matches.map((m) => {
      const lineNum = src.slice(0, m.index).split('\n').length;
      return `  line ${lineNum}: ${lines[lineNum - 1].trim()}`;
    });
    fail(
      `setVideoQuality() found in ${path.relative(GUI_DIR, sourceFile)} (R11.7, R17.1).\n` +
      `setVideoQuality is a no-op on VP9 SVC tracks and must not be called:\n` +
      hitLines.join('\n'),
    );
  }
  log('No setVideoQuality calls found in livekit-media.ts');
}

function assertTransceiverReusePatchPresent() {
  if (!fs.existsSync(LK_ESM_PATH)) {
    fail(
      `livekit-client ESM not found at ${LK_ESM_PATH}.\n` +
      `Run 'npm install' in clients/wavis-gui to install dependencies.`,
    );
  }
  const esm = fs.readFileSync(LK_ESM_PATH, 'utf8');
  try {
    verifyPatchMarkers(esm);
  } catch (err) {
    fail(
      `Transceiver-reuse patch not applied to bundled livekit-client (R7.3, R17.5).\n` +
      `Run 'npm run patch:livekit' in clients/wavis-gui before starting the runner.\n` +
      `Details: ${err.message}`,
    );
  }
  log('Transceiver-reuse patch markers verified in bundled livekit-client');
}

function assertFixturesPresent(cells, skipFixtureCheck) {
  const requiredSources = [...new Set(cells.map((c) => c.contentSource))];
  for (const source of requiredSources) {
    const fixturePath = path.resolve(FIXTURES_DIR, source);
    if (!fs.existsSync(fixturePath)) {
      if (skipFixtureCheck) {
        log(`WARN: fixture directory missing (${fixturePath}) — skipped via --skip-fixture-check. You must manually set up ${source} content during each cell.`);
      } else {
        fail(
          `Content corpus fixture missing: ${fixturePath}\n` +
          `See artifacts/codec-shootout/fixtures/README.md for setup instructions.\n` +
          `Use --skip-fixture-check to run with manual content instead.`,
        );
      }
    }
  }
  if (!skipFixtureCheck) log(`Content corpus fixtures verified for sources: ${requiredSources.join(', ')}`);
}

/* ─── Run-directory setup ────────────────────────────────────────── */

function buildRunId() {
  const now = new Date();
  const ts = now.toISOString().replace(/[:.]/g, '-').slice(0, 19);
  return `run-${ts}`;
}

function setupRunDir(runId) {
  const runDir = path.resolve(ARTIFACTS_BASE, 'runs', runId);
  fs.mkdirSync(runDir, { recursive: true });
  return runDir;
}

/* ─── Subjective scoring template (R11.3 / §4.4) ───────────────── */

function writeSubjectiveTemplate(runDir, runId, cells) {
  const rows = [
    'runId,cellId,codecPrimary,participants,contentSource,profile,raterName,codeLegibility1to5,motionSmoothness1to5,notes',
  ];
  for (const cell of cells) {
    rows.push(
      `${runId},${cell.cellId},${cell.codecPolicy.primary},${cell.participants},${cell.contentSource},${cell.profile},,,,`,
    );
  }
  const templatePath = path.resolve(runDir, 'subjective.csv');
  fs.writeFileSync(templatePath, rows.join('\n') + '\n');
  log(`Subjective scoring template written to ${templatePath}`);
  return templatePath;
}

/* ─── Per-cell JSONL writer ──────────────────────────────────────── */

function createCellSampleWriter(cellDir, cellId) {
  fs.mkdirSync(cellDir, { recursive: true });
  const jsonlPath = path.resolve(cellDir, `${cellId}.jsonl`);
  const stream = fs.createWriteStream(jsonlPath, { flags: 'a' });

  return {
    write(sample) {
      stream.write(JSON.stringify(sample) + '\n');
    },
    close() {
      return new Promise((resolve) => stream.end(resolve));
    },
    path: jsonlPath,
  };
}

/* ─── LiveKit room creation ──────────────────────────────────────── */

async function createLiveKitRoom(args, roomName) {
  // LiveKit room creation via the Server SDK REST API.
  // The runner uses a fetch call to POST /twirp/livekit.RoomService/CreateRoom.
  const { livekitUrl, livekitApiKey, livekitApiSecret } = args;
  if (!livekitUrl || !livekitApiKey || !livekitApiSecret) {
    log(`WARN: LiveKit credentials not provided — skipping room creation (dry-run or test mode)`);
    return { name: roomName };
  }

  // Build a minimal LiveKit API token for the server SDK call.
  // In a full implementation this uses the livekit-server-sdk or a raw JWT.
  // For now: delegate to an external lk CLI command if available.
  try {
    const httpUrl = livekitUrl.replace(/^wss?:/, 'https:');
    const response = await fetch(`${httpUrl}/twirp/livekit.RoomService/CreateRoom`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Authorization: buildLiveKitAdminToken(livekitApiKey, livekitApiSecret),
      },
      body: JSON.stringify({
        name: roomName,
        empty_timeout: 60,
        max_participants: 6,
      }),
    });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}: ${await response.text()}`);
    }
    return await response.json();
  } catch (err) {
    log(`WARN: Could not create LiveKit room via API: ${err.message}`);
    return { name: roomName };
  }
}

function buildLiveKitAdminToken(apiKey, apiSecret) {
  const header = Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })).toString('base64url');
  const now = Math.floor(Date.now() / 1000);
  const payload = Buffer.from(JSON.stringify({
    iss: apiKey,
    exp: now + 600,
    nbf: now,
    video: { roomCreate: true, roomList: true, roomAdmin: true },
  })).toString('base64url');
  const sig = createHmac('sha256', apiSecret)
    .update(`${header}.${payload}`)
    .digest('base64url');
  return `Bearer ${header}.${payload}.${sig}`;
}

/* ─── Publisher process ──────────────────────────────────────────── */

function spawnPublisher(args, cell, runId, roomName, runDir) {
  if (!args.appBinary) {
    log(`WARN: --app-binary not provided — publisher process not spawned (dry-run)`);
    return null;
  }

  const env = {
    ...process.env,
    WAVIS_SHOOTOUT_ENABLED: '1',
    WAVIS_SHOOTOUT_RUN_ID: runId,
    WAVIS_SHOOTOUT_CELL_ID: cell.cellId,
    WAVIS_SHOOTOUT_ROOM_NAME: roomName,
    WAVIS_SHOOTOUT_CODEC_PRIMARY: cell.codecPolicy.primary,
    WAVIS_SHOOTOUT_CODEC_BACKUP: cell.codecPolicy.backup ?? '',
    WAVIS_SHOOTOUT_SIMULCAST: String(cell.codecPolicy.simulcast),
    WAVIS_SHOOTOUT_PROFILE: cell.profile,
    WAVIS_SHOOTOUT_CONTENT_SOURCE: cell.contentSource,
    WAVIS_SHOOTOUT_ARTIFACT_DIR: runDir,
    WAVIS_SHOOTOUT_FIXTURE_DIR: FIXTURES_DIR,
    WAVIS_SHOOTOUT_PARTICIPANT_COUNT: String(cell.participants),
    WAVIS_LIVEKIT_URL: args.livekitUrl ?? '',
    // Never set VITE_WAVIS_WINDOWS_SHARE_PATH in a production build — the
    // release-config check enforces this. The harness uses the env var for
    // path override only in development builds.
  };

  const proc = spawn(args.appBinary, [], { env, stdio: 'pipe' });
  proc.stdout.on('data', (d) => process.stdout.write(`[publisher] ${d}`));
  proc.stderr.on('data', (d) => process.stderr.write(`[publisher] ${d}`));
  log(`Publisher spawned (PID ${proc.pid}) for cell ${cell.cellId}`);
  return proc;
}

/* ─── Subscriber process ─────────────────────────────────────────── */

function spawnSubscriber(args, cell, runId, roomName, subscriberIndex, runDir) {
  const subscriberScript = path.resolve(__dirname, 'subscriber.mjs');
  if (!fs.existsSync(subscriberScript)) {
    log(`WARN: subscriber.mjs not found — synthetic subscriber ${subscriberIndex} not spawned`);
    return null;
  }

  const env = {
    ...process.env,
    WAVIS_SHOOTOUT_ENABLED: '1',
    WAVIS_SHOOTOUT_RUN_ID: runId,
    WAVIS_SHOOTOUT_CELL_ID: cell.cellId,
    WAVIS_SHOOTOUT_ROOM_NAME: roomName,
    WAVIS_SHOOTOUT_SUBSCRIBER_INDEX: String(subscriberIndex),
    WAVIS_SHOOTOUT_ARTIFACT_DIR: runDir,
    WAVIS_LIVEKIT_URL: args.livekitUrl ?? '',
    WAVIS_LIVEKIT_API_KEY: args.livekitApiKey ?? '',
    WAVIS_LIVEKIT_API_SECRET: args.livekitApiSecret ?? '',
  };

  const proc = spawn(process.execPath, [subscriberScript], { env, stdio: 'pipe' });
  proc.stdout.on('data', (d) => process.stdout.write(`[sub-${subscriberIndex}] ${d}`));
  proc.stderr.on('data', (d) => process.stderr.write(`[sub-${subscriberIndex}] ${d}`));
  log(`Subscriber ${subscriberIndex} spawned (PID ${proc.pid})`);
  return proc;
}

/* ─── Cell runner ────────────────────────────────────────────────── */

async function runCell(args, cell, runId, runDir) {
  const roomName = `shootout-${runId}-${cell.cellId}`;
  log(`\n── Cell ${cell.cellId} ──────────────────────────`);
  log(`  Codec: ${JSON.stringify(cell.codecPolicy)}`);
  log(`  Profile: ${cell.profile}, Content: ${cell.contentSource}, Participants: ${cell.participants}`);
  log(`  Duration: ${cell.durationSeconds}s (excluding ${15}s ramp)`);

  if (args.dryRun) {
    log(`  [DRY RUN] Would create room ${roomName} and spawn processes`);
    return { cellId: cell.cellId, status: 'dry_run', roomName };
  }

  await createLiveKitRoom(args, roomName);

  const cellDir = path.resolve(runDir, cell.cellId);
  const writer = createCellSampleWriter(cellDir, cell.cellId);
  const processes = [];

  // Write cell metadata as the first JSONL entry
  writer.write({
    type: 'cell_start',
    runId,
    cellId: cell.cellId,
    timestampMs: Date.now(),
    codec: cell.codecPolicy,
    profile: cell.profile,
    participantCount: cell.participants,
    content: cell.contentSource,
    durationSeconds: cell.durationSeconds,
    rampExclusionSeconds: 15,
  });

  // Spawn publisher
  const publisher = spawnPublisher(args, cell, runId, roomName, runDir);
  if (publisher) processes.push(publisher);

  // Spawn N-1 synthetic subscribers (participants - 1 since publisher counts as 1)
  const subscriberCount = cell.participants - 1;
  for (let i = 0; i < subscriberCount; i++) {
    const sub = spawnSubscriber(args, cell, runId, roomName, i + 1, runDir);
    if (sub) processes.push(sub);
  }

  // Wait for cell duration (ramp + steady-state)
  const totalMs = (cell.durationSeconds + 15) * 1000;
  log(`  Waiting ${cell.durationSeconds + 15}s (15s ramp + ${cell.durationSeconds}s steady state)...`);
  await sleep(totalMs);

  // Write cell end marker
  writer.write({
    type: 'cell_end',
    runId,
    cellId: cell.cellId,
    timestampMs: Date.now(),
  });

  // Terminate all processes
  for (const proc of processes) {
    if (proc && !proc.killed) {
      proc.kill('SIGTERM');
    }
  }
  await sleep(2000); // allow graceful shutdown

  await writer.close();
  log(`  Cell ${cell.cellId} complete — JSONL at ${writer.path}`);
  return { cellId: cell.cellId, status: 'complete', jsonlPath: writer.path, roomName };
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/* ─── Aggregate CSV (§4.4 outputs) ──────────────────────────────── */

function buildAggregateCsv(runId, cellResults) {
  const rows = [
    'runId,cellId,codecPrimary,codecBackup,simulcast,participants,contentSource,profile,status,jsonlPath',
  ];
  for (const r of cellResults) {
    rows.push(
      [runId, r.cellId, r.codecPrimary ?? '', r.codecBackup ?? '', r.simulcast ?? '', r.participants ?? '', r.contentSource ?? '', r.profile ?? '', r.status, r.jsonlPath ?? ''].join(','),
    );
  }
  return rows.join('\n') + '\n';
}

/* ─── Main ───────────────────────────────────────────────────────── */

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const runId = args.runId ?? buildRunId();

  log(`Codec-policy shootout runner — run ID: ${runId}`);
  log(`Repo root: ${REPO_ROOT}`);

  // 1. Entry-condition gate (R11.1)
  checkEntryConditions(args.skipW2Gate);

  // 2. Load matrix
  if (!fs.existsSync(MATRIX_PATH)) {
    fail(`matrix.json not found at ${MATRIX_PATH}`);
  }
  const matrix = JSON.parse(fs.readFileSync(MATRIX_PATH, 'utf8'));
  let cells = matrix.cells;

  // 2a. Apply cell filter if provided
  if (args.cellFilter) {
    cells = cells.filter((c) => args.cellFilter.has(c.cellId));
    if (cells.length === 0) {
      fail(`No cells matched the filter: ${[...args.cellFilter].join(', ')}`);
    }
    log(`Cell filter applied — running ${cells.length}/${matrix.cells.length} cells`);
  }

  log(`Matrix loaded: ${cells.length} cells`);

  // 3. Pre-run anti-pattern assertions (8.3 / R11.6, R11.7, R7.3/R17.5)
  log('\nRunning pre-run assertions...');
  assertDynacaseDisabledInMatrix(cells);
  assertNoSetVideoQualityOnVp9(LK_MEDIA_SRC);
  assertTransceiverReusePatchPresent();
  if (!args.dryRun) {
    assertFixturesPresent(cells, args.skipFixtureCheck);
  }
  log('All pre-run assertions passed\n');

  // 4. Setup run directory
  const runDir = setupRunDir(runId);
  log(`Run directory: ${runDir}`);

  // Write run manifest
  fs.writeFileSync(
    path.resolve(runDir, 'manifest.json'),
    JSON.stringify({ runId, startedAt: new Date().toISOString(), cells: cells.map((c) => c.cellId), args: { appBinary: args.appBinary, livekitUrl: args.livekitUrl, dryRun: args.dryRun } }, null, 2),
  );

  // 5. Generate subjective scoring template (§4.4 / R11.3)
  const subjectivePath = writeSubjectiveTemplate(runDir, runId, cells);
  log(`Subjective scoring template: ${subjectivePath}`);

  // 6. Run cells sequentially
  log(`\nStarting ${cells.length} cells...`);
  const cellResults = [];
  for (const cell of cells) {
    try {
      const result = await runCell(args, cell, runId, runDir);
      cellResults.push({
        ...result,
        codecPrimary: cell.codecPolicy.primary,
        codecBackup: cell.codecPolicy.backup ?? '',
        simulcast: cell.codecPolicy.simulcast,
        participants: cell.participants,
        contentSource: cell.contentSource,
        profile: cell.profile,
      });
    } catch (err) {
      log(`ERROR in cell ${cell.cellId}: ${err.message}`);
      cellResults.push({ cellId: cell.cellId, status: 'error', error: err.message });
    }
  }

  // 7. Write aggregate CSV
  const csvPath = path.resolve(runDir, 'summary.csv');
  fs.writeFileSync(csvPath, buildAggregateCsv(runId, cellResults));
  log(`\nAggregate CSV written to ${csvPath}`);

  // 8. Post-run summary
  const completed = cellResults.filter((r) => r.status === 'complete' || r.status === 'dry_run').length;
  const errored = cellResults.filter((r) => r.status === 'error').length;
  log(`\n── Run complete ───────────────────────────────`);
  log(`  Run ID: ${runId}`);
  log(`  Cells: ${completed} completed, ${errored} errored`);
  log(`  Artifacts: ${runDir}`);
  log(`  Subjective scoring: ${subjectivePath}`);
  log(`\nNext step: fill in ${path.basename(subjectivePath)} with blind Likert scores,`);
  log(`then record the W4 codec-policy decision (vp9vp8 | vp8sim | av1vp9) per design.md §4.4 outcome handling.`);

  if (errored > 0) {
    process.exit(1);
  }
}

main().catch((err) => {
  console.error('[shootout] Unhandled error:', err);
  process.exit(1);
});
