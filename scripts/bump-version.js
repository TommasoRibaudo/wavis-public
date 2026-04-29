#!/usr/bin/env node
// Usage: node scripts/bump-version.js <version>
//
// Updates the version field in both:
//   clients/wavis-gui/src-tauri/tauri.conf.json
//   clients/wavis-gui/package.json
//
// Run this before committing and tagging a release:
//   node scripts/bump-version.js 0.2.0
//   git add clients/wavis-gui/src-tauri/tauri.conf.json clients/wavis-gui/package.json
//   git commit -m "chore: bump version to 0.2.0"
//   git tag desktop-v0.2.0
//   git push && git push --tags

import { readFileSync, writeFileSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, '..');

const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
  console.error('Usage: node scripts/bump-version.js <version>  (e.g. 0.2.0)');
  process.exit(1);
}

function bump(filePath, mutate) {
  const raw = readFileSync(filePath, 'utf8');
  const obj = JSON.parse(raw);
  const old = obj.version;
  mutate(obj);
  writeFileSync(filePath, JSON.stringify(obj, null, 2) + '\n');
  console.log(`${filePath.replace(root + '/', '')}:  ${old} → ${obj.version}`);
}

bump(join(root, 'clients/wavis-gui/src-tauri/tauri.conf.json'), o => { o.version = version; });
bump(join(root, 'clients/wavis-gui/package.json'),              o => { o.version = version; });

console.log(`\nNext steps:`);
console.log(`  git add clients/wavis-gui/src-tauri/tauri.conf.json clients/wavis-gui/package.json`);
console.log(`  git commit -m "chore: bump version to ${version}"`);
console.log(`  git tag desktop-v${version}`);
console.log(`  git push && git push --tags`);
