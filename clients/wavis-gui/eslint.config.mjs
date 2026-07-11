// ESLint policy for wavis-gui.
//
// Severity policy (see CLAUDE.md "Code Standards & Gates"):
// - error  = CI-blocking correctness/boundary rules
// - warn   = review triggers (complexity, purity, type-safety pressure);
//            never blocking, upgraded per-rule once the codebase is clean
//
// Formatting is Prettier's job — no style rules here.

import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import reactHooks from 'eslint-plugin-react-hooks';

// UI components (.tsx) that imported @tauri-apps/* before the fence existed.
// Baseline allowlist: burn down by routing through a bridge/adapter module
// (see src/shared/*-bridge.ts, src/features/voice/native-media.ts for the
// pattern). No new entries without a bridge-first attempt.
const TAURI_IMPORT_BASELINE = [
  'src/App.tsx',
  'src/features/channels/ChannelDetail.tsx',
  'src/features/diagnostics/BugReportButton.tsx',
  'src/features/diagnostics/DiagnosticsPage.tsx',
  'src/features/screen-share/ScreenSharePage.tsx',
  'src/features/screen-share/ShareIndicator.tsx',
  'src/features/screen-share/SharePicker.tsx',
  'src/features/screen-share/WatchAllPage.tsx',
  'src/features/settings/Settings.tsx',
  'src/features/voice/ActiveRoom.tsx',
  'src/features/voice/VideoPopoutPage.tsx',
  'src/shared/AppUpdatePrompt.tsx',
  'src/shared/TitleBar.tsx',
];

export default tseslint.config(
  {
    ignores: [
      'dist/**',
      'src-tauri/**',
      'scripts/**',
      'artifacts/**',
      'src/features/voice/linux-capability-matrix.ts', // generated
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  {
    files: ['src/**/*.{ts,tsx}'],
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: {
      'react-hooks': reactHooks,
    },
    rules: {
      // correctness — blocking
      'react-hooks/rules-of-hooks': 'error',
      '@typescript-eslint/no-unused-vars': [
        'error',
        {
          argsIgnorePattern: '^_',
          varsIgnorePattern: '^_',
          caughtErrorsIgnorePattern: '^_',
          ignoreRestSiblings: true,
        },
      ],
      // disallowTypeAnnotations:false — `typeof import('...')` annotations are
      // the idiom for typing lazily/dynamically imported modules (App.tsx lazy
      // voice-room access, vi.resetModules() test harnesses).
      '@typescript-eslint/consistent-type-imports': [
        'error',
        { prefer: 'type-imports', disallowTypeAnnotations: false },
      ],

      // Ratcheted to error 2026-07-10 (violation count reached zero).
      // Ratchet policy: a warn rule graduates here once its count is zero;
      // it never goes back.
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/restrict-template-expressions': 'error',
      '@typescript-eslint/no-redundant-type-constituents': 'error',
      '@typescript-eslint/no-unsafe-enum-comparison': 'error',
      '@typescript-eslint/no-base-to-string': 'error',
      '@typescript-eslint/no-this-alias': 'error', // off in tests (vi.mock `this` captures)
      '@typescript-eslint/unbound-method': 'error', // off in tests (vi.mocked false positive)
      'preserve-caught-error': 'error',
      'no-async-promise-executor': 'error',

      // React design pressure — review triggers
      'react-hooks/exhaustive-deps': 'warn',
      'react-hooks/set-state-in-effect': 'warn',
      // purity: 1 known hit — vendored shadcn sidebar skeleton uses
      // Math.random for decorative width; not worth forking vendored UI over.
      'react-hooks/purity': 'warn',

      // TS design pressure — review triggers
      '@typescript-eslint/no-unnecessary-condition': 'warn',

      // Typed-strictness rules from recommendedTypeChecked demoted to warn:
      // ~900 pre-existing violations (Tauri invoke() returns any, LiveKit
      // event payloads, fire-and-forget async). Review triggers for now;
      // upgrade per-rule once its count reaches zero.
      '@typescript-eslint/require-await': 'warn',
      '@typescript-eslint/no-floating-promises': 'warn',
      '@typescript-eslint/no-misused-promises': 'warn',
      '@typescript-eslint/no-unsafe-assignment': 'warn',
      '@typescript-eslint/no-unsafe-member-access': 'warn',
      '@typescript-eslint/no-unsafe-call': 'warn',
      '@typescript-eslint/no-unsafe-return': 'warn',
      '@typescript-eslint/no-unsafe-argument': 'warn',
      '@typescript-eslint/only-throw-error': 'warn',
      // prefer-promise-reject-errors: 2 known hits in withTimeout()
      // (livekit-media.ts) — rejecting with a typed CameraStartError sentinel
      // is an intentional contract discriminated by isCameraStartError().
      '@typescript-eslint/prefer-promise-reject-errors': 'warn',
      'no-useless-assignment': 'warn',
      // Empty catch is an established intentional pattern here
      // (best-effort native calls); empty non-catch blocks still error.
      'no-empty': ['error', { allowEmptyCatch: true }],

      // soft complexity signals — review triggers, never gates
      complexity: ['warn', 15],
      'max-lines-per-function': [
        'warn',
        { max: 120, skipBlankLines: true, skipComments: true, IIFEs: true },
      ],
    },
  },
  {
    // Plain JS (audio worklets) — no TS project info available, and the
    // AudioWorkletGlobalScope globals are not in any standard env.
    files: ['src/**/*.js'],
    ...tseslint.configs.disableTypeChecked,
    languageOptions: {
      globals: {
        AudioWorkletProcessor: 'readonly',
        registerProcessor: 'readonly',
        sampleRate: 'readonly',
        currentTime: 'readonly',
        currentFrame: 'readonly',
      },
    },
  },
  {
    // Boundary fence: UI components must reach Tauri through bridge/adapter
    // modules (.ts), never directly. Plain .ts modules are the adapter layer
    // and may import @tauri-apps freely.
    files: ['src/**/*.tsx'],
    ignores: TAURI_IMPORT_BASELINE,
    rules: {
      'no-restricted-imports': [
        'error',
        {
          patterns: [
            {
              group: ['@tauri-apps/*'],
              message:
                'UI components must not import Tauri APIs directly — add or reuse a bridge module (e.g. src/shared/notification-bridge.ts) and import that.',
            },
          ],
        },
      ],
    },
  },
  {
    // Tests mock Tauri modules and legitimately reference their specifiers;
    // relax type-safety pressure rules that fight test doubles.
    files: ['src/**/*.test.{ts,tsx}', 'src/**/__tests__/**/*.{ts,tsx}'],
    rules: {
      'no-restricted-imports': 'off',
      '@typescript-eslint/no-explicit-any': 'off',
      'max-lines-per-function': 'off',
      // vi.mocked(obj.method) / expect(obj.method) are the documented
      // unbound-method false positive in vitest/jest suites.
      '@typescript-eslint/unbound-method': 'off',
      // vi.mock constructor doubles capture `this` into module-scope vars
      // (lastLkModule = this) so tests can assert on the built instance.
      '@typescript-eslint/no-this-alias': 'off',
    },
  },
);
