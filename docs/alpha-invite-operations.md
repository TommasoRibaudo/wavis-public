# Closed-Alpha Invite Operations

The `alpha-invite-admin` binary creates and manages per-user registration
invites without exposing an administrative HTTP endpoint. It connects directly
to Postgres, stores only an HMAC-SHA256 hash, and prints a newly generated raw
code once after the database insert succeeds.

## Prerequisites

Run the backend migrations before using the tool. The operator environment must
have network access to Postgres and these variables:

- `DATABASE_URL`: the target Postgres connection string.
- `ALPHA_INVITE_CODE_PEPPER`: the same value used by the target backend. It
  must be at least 32 bytes and is needed only by `create`.

Keep both values in environment variables. Do not pass them as command-line
arguments, commit them, or paste them into tickets and logs.

The tool also loads a `.env` file from the working directory if present, the
same way the backend does. If `DATABASE_URL` is not exported, a repo-root
`.env` can silently point the command at a development database — confirm
which database you are targeting before creating or disabling invites.

## Create an invite

Labels are operator metadata, can be reused, and are never presented to the
registrant. A new invite allows one redemption and has no expiry unless the
optional flags are supplied.

### Bash

```bash
export DATABASE_URL='postgres://...'
export ALPHA_INVITE_CODE_PEPPER='the-same-32-byte-minimum-backend-pepper'

cargo run -p wavis-backend --bin alpha-invite-admin -- create \
  --label 'person@example.com' \
  --expires-at '2030-08-01T23:59:59Z' \
  --max-redemptions 1
```

### PowerShell

```powershell
$env:DATABASE_URL = 'postgres://...'
$env:ALPHA_INVITE_CODE_PEPPER = 'the-same-32-byte-minimum-backend-pepper'

cargo run -p wavis-backend --bin alpha-invite-admin -- create --label 'person@example.com' --expires-at '2030-08-01T23:59:59Z' --max-redemptions 1
```

The response contains an `invite_id` and one `invite_code` line. Record the
UUID for later operations and send the raw code immediately through an approved
private channel. The raw code cannot be inspected or recovered later. Avoid
placing it in issue trackers, shared documents, shell history, or application
logs.

## Inspect invites

Listing never returns raw codes or hashes. Omit `--label` to list every invite,
or supply an exact label to narrow the result.

```bash
cargo run -p wavis-backend --bin alpha-invite-admin -- list
cargo run -p wavis-backend --bin alpha-invite-admin -- list --label 'person@example.com'
```

```powershell
cargo run -p wavis-backend --bin alpha-invite-admin -- list
cargo run -p wavis-backend --bin alpha-invite-admin -- list --label 'person@example.com'
```

The status is one of `active`, `disabled`, `expired`, or `exhausted`.
`remaining` is the maximum count minus recorded redemptions.

## Disable an invite

Use the UUID returned by `create` or `list`. Disabling is idempotent: repeating
the command leaves the original disable time unchanged.

```bash
cargo run -p wavis-backend --bin alpha-invite-admin -- disable \
  --id '00000000-0000-0000-0000-000000000000'
```

```powershell
cargo run -p wavis-backend --bin alpha-invite-admin -- disable --id '00000000-0000-0000-0000-000000000000'
```

If a raw code is lost or may have been exposed, disable its UUID and create a
replacement. There is no code-recovery operation by design.

## Shared dev and CI invite

`scripts/seed-alpha-invite.py` remains available for the existing fixed-code
dev/CI bootstrap flow. It accepts a caller-provided raw code and is not the
per-user issuance workflow. Use `alpha-invite-admin create` for all individual
alpha invitations.
