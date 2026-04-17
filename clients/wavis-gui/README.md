# Wavis GUI

Tauri 2.0 + React desktop client. All commands run from `clients/wavis-gui/`.

## Prerequisites

- Node.js (LTS) + npm
- Rust toolchain (rustup)
- Tauri CLI: included as a dev dependency (`@tauri-apps/cli`)
- Platform build tools: [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)

## Setup

```sh
npm install
```

## Commands

| Command | What it does |
|---------|-------------|
| `npx tauri dev` | Dev mode — opens the app with hot reload (Vite + Rust) |
| `npx tauri build` | Production build — compiles the app into a native installer |
| `npm run build` | Frontend only — runs `tsc` + `vite build` (no Rust) |
| `npm run dev` | Frontend only — starts Vite dev server (no Tauri shell) |
| `npm run test` | Runs Vitest unit tests (single run) |

## Notes

- `npx tauri dev` starts both the Vite dev server and the Rust backend, then opens the native window. First run compiles the Rust side which takes a while.
- `npx tauri build` produces platform-specific installers in `src-tauri/target/release/bundle/`.
- Don't pass extra path arguments to `npx tauri build` — it runs from the current directory automatically.
- The Tauri Rust code lives in `src-tauri/`. Frontend source is in `src/`.
