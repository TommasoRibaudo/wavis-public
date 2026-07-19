# Test Harnesses

## GUI Surface Test

Exercises backend REST endpoints the GUI client depends on. Requires Postgres and a running backend built with the `test-metrics` feature (for rate-limit reset between scenarios).

### PowerShell (Windows)

```powershell
# 1. Start Postgres
docker compose up -d postgres

# 2. Terminal 1 — start the backend
$env:TEST_METRICS_TOKEN="dev-token"; $env:DATABASE_URL="postgres://wavis:wavis@localhost:5432/wavis"; cargo run -p wavis-backend --features test-metrics

# 3. Terminal 2 — run the tests
$env:TEST_METRICS_TOKEN="dev-token"; cargo run -p gui-surface-test -- --url http://127.0.0.1:3000
```

### Bash (Linux/macOS)

```bash
# 1. Start Postgres
docker compose up -d postgres

# 2. Terminal 1 — start the backend
DATABASE_URL=postgres://wavis:wavis@localhost:5432/wavis TEST_METRICS_TOKEN=dev-token cargo run -p wavis-backend --features test-metrics

# 3. Terminal 2 — run the tests
TEST_METRICS_TOKEN=dev-token cargo run -p gui-surface-test -- --url http://127.0.0.1:3000
```

## Stress Test

### In-process (quick, no external dependencies)

Starts its own backend internally with a mock SFU and dummy Postgres pool.
Most scenarios run, but DB-dependent ones (auth-state-machine-race, cross-secret-token-confusion, refresh-token-reuse) are skipped. Stop any running backend on port 3000 first.

```bash
cargo run -p stress-harness -- --in-process --ci
```

### In-process with Postgres (full coverage)

Same as above but connects to a real Postgres instance, enabling all DB-dependent auth scenarios. This is the recommended way to run the full suite.

#### Bash (Linux/macOS)

```bash
# 1. Start Postgres
docker compose up -d postgres

# 2. Run the stress harness
DATABASE_URL=postgres://wavis:wavis@localhost:5432/wavis cargo run -p stress-harness -- --in-process --ci
```

#### PowerShell (Windows)

```powershell
# 1. Start Postgres
docker compose up -d postgres

# 2. Run the stress harness
$env:DATABASE_URL="postgres://wavis:wavis@localhost:5432/wavis"; cargo run -p stress-harness -- --in-process --ci
```

### External backend (advanced)

Run against a separately started backend. This enables DB-dependent scenarios but cannot tweak rate limiter config at runtime, so some rate-limiter-sensitive scenarios may behave differently.

#### Bash (Linux/macOS)

```bash
# 1. Start Postgres
docker compose up -d postgres

# 2. Terminal 1 — start the backend
DATABASE_URL=postgres://wavis:wavis@localhost:5432/wavis TEST_METRICS_TOKEN=dev-token cargo run -p wavis-backend --features test-metrics

# 3. Terminal 2 — run the stress harness
TEST_METRICS_TOKEN=dev-token cargo run -p stress-harness -- --ci
```

#### PowerShell (Windows)

```powershell
# 1. Start Postgres
docker compose up -d postgres

# 2. Terminal 1 — start the backend
$env:TEST_METRICS_TOKEN="dev-token"; $env:DATABASE_URL="postgres://wavis:wavis@localhost:5432/wavis"; cargo run -p wavis-backend --features test-metrics

# 3. Terminal 2 — run the stress harness
$env:TEST_METRICS_TOKEN="dev-token"; cargo run -p stress-harness -- --ci
```
