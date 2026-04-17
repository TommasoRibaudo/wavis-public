#!/usr/bin/env bash
# pg-backup-s3.sh — Dump the Postgres database and upload to S3.
# Run via SSM Session Manager BEFORE creating an AMI for instance migration.
#
# Usage:
#   bash deploy/pg-backup-s3.sh                          # uses default bucket
#   S3_BACKUP_BUCKET=my-bucket bash deploy/pg-backup-s3.sh
#
# Requirements: 5.6

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
S3_BACKUP_BUCKET="${S3_BACKUP_BUCKET:-wavis-backups-dev}"
REGION="${AWS_REGION:-us-east-2}"
CONTAINER_NAME="${PG_CONTAINER:-wavis-postgres}"
PG_USER="${PG_USER:-wavis}"
PG_DB="${PG_DB:-wavis}"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
S3_KEY="pg-dump-${TIMESTAMP}.sql.gz"
DUMP_FILE="$(mktemp /tmp/pg-dump-XXXXXX.sql.gz)"

# Clean up temp file on exit (success or failure).
trap 'rm -f "${DUMP_FILE}"' EXIT

echo "=== Wavis Postgres Backup ==="
echo "Container: ${CONTAINER_NAME}"
echo "Database:  ${PG_DB}"
echo "S3 target: s3://${S3_BACKUP_BUCKET}/${S3_KEY}"

# ---------------------------------------------------------------------------
# 1. Verify the Postgres container is running
# ---------------------------------------------------------------------------
if ! docker inspect --format='{{.State.Running}}' "${CONTAINER_NAME}" 2>/dev/null | grep -q true; then
  echo "ERROR: Container '${CONTAINER_NAME}' is not running." >&2
  echo "Start it with: docker compose up -d postgres" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 2. Run pg_dump inside the container and compress
# ---------------------------------------------------------------------------
echo "Running pg_dump..."
docker exec "${CONTAINER_NAME}" pg_dump -U "${PG_USER}" "${PG_DB}" | gzip > "${DUMP_FILE}"

DUMP_SIZE=$(stat --printf='%s' "${DUMP_FILE}" 2>/dev/null || stat -f '%z' "${DUMP_FILE}")
echo "Dump size: ${DUMP_SIZE} bytes (compressed)"

if [[ "${DUMP_SIZE}" -eq 0 ]]; then
  echo "ERROR: pg_dump produced an empty file." >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 3. Upload to S3
# ---------------------------------------------------------------------------
echo "Uploading to s3://${S3_BACKUP_BUCKET}/${S3_KEY}..."
aws s3 cp "${DUMP_FILE}" "s3://${S3_BACKUP_BUCKET}/${S3_KEY}" --region "${REGION}"

echo ""
echo "=== Backup complete ==="
echo "Restore with:"
echo "  aws s3 cp s3://${S3_BACKUP_BUCKET}/${S3_KEY} - | gunzip | docker exec -i ${CONTAINER_NAME} psql -U ${PG_USER} ${PG_DB}"
