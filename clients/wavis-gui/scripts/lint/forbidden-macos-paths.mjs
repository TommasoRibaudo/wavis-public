import fs from 'node:fs';
import path from 'node:path';

const ROOTS = [
  path.resolve('src-tauri/src/audio_capture'),
  path.resolve('src-tauri/src/share_sources'),
];

const ALLOW_START = 'wavis-lint-allow-sck-fallback-start';
const ALLOW_END = 'wavis-lint-allow-sck-fallback-end';

const CHECKS = [
  {
    id: 'bundleIdentifier',
    regex: /\bbundleIdentifier\b/,
    allowInSckFallback: true,
    message:
      'bundle-identifier based macOS exclusion is only allowed inside the ScreenCaptureKit fallback branch.',
  },
  {
    id: 'SCRunningApplications',
    regex: /\bSCRunningApplication(?:s)?\b/,
    allowInSckFallback: true,
    message: 'SCRunningApplications may only be used inside the ScreenCaptureKit fallback branch.',
  },
  {
    id: 'getppid',
    regex: /\bgetppid\b/,
    allowInSckFallback: false,
    message: 'parent-process-ID based macOS exclusion is forbidden.',
  },
  {
    id: 'pgrep-parent',
    regex: /Command::new\("pgrep"\)|\.arg\("-P"\)|pgrep\s+-P/,
    allowInSckFallback: false,
    message: 'pgrep -P / parent-process-ID based macOS exclusion is forbidden.',
  },
];

function fail(message) {
  console.error(`[forbidden-macos-paths] ${message}`);
  process.exit(1);
}

function listFiles(rootDir) {
  const files = [];
  for (const entry of fs.readdirSync(rootDir, { withFileTypes: true })) {
    const fullPath = path.join(rootDir, entry.name);
    if (entry.isDirectory()) {
      files.push(...listFiles(fullPath));
      continue;
    }
    if (entry.isFile() && /\.(rs|ts|tsx|js|mjs)$/.test(entry.name)) {
      files.push(fullPath);
    }
  }
  return files;
}

function isCommentOnlyLine(line) {
  const trimmed = line.trim();
  return trimmed.startsWith('//') || trimmed.startsWith('///');
}

function scanFile(filePath) {
  const content = fs.readFileSync(filePath, 'utf8');
  const lines = content.split(/\r?\n/);
  const violations = [];
  let allowSckFallback = false;

  lines.forEach((line, index) => {
    if (line.includes(ALLOW_START)) {
      allowSckFallback = true;
    }
    if (!isCommentOnlyLine(line)) {
      for (const check of CHECKS) {
        if (!check.regex.test(line)) {
          continue;
        }
        if (check.allowInSckFallback && allowSckFallback) {
          continue;
        }
        violations.push(`${path.relative(process.cwd(), filePath)}:${index + 1} ${check.message}`);
      }
    }
    if (line.includes(ALLOW_END)) {
      allowSckFallback = false;
    }
  });

  return violations;
}

const violations = ROOTS.flatMap((rootDir) => listFiles(rootDir).flatMap(scanFile));

if (violations.length > 0) {
  fail(violations.join('\n'));
}

console.log('[forbidden-macos-paths] OK');
