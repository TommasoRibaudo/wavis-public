import fs from 'node:fs';
import path from 'node:path';
import { verifyPatchMarkers } from './apply-livekit-transceiver-reuse-fix.mjs';

const ESM_PATH = path.resolve('node_modules/livekit-client/dist/livekit-client.esm.mjs');

function fail(message) {
  console.error(`[livekit-fix] ${message}`);
  process.exit(1);
}

if (!fs.existsSync(ESM_PATH)) {
  fail(`missing bundled livekit-client ESM at ${ESM_PATH}`);
}

const esm = fs.readFileSync(ESM_PATH, 'utf8');
verifyPatchMarkers(esm);
console.log('[livekit-fix] verified patch markers');
