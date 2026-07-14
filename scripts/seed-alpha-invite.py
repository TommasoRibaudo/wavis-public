#!/usr/bin/env python3
"""Seed a closed-alpha invite row.

By default this prints SQL that can be piped to psql. With --execute it invokes
psql directly using DATABASE_URL.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import os
import subprocess
import sys

HASH_DOMAIN = b"alpha-invite:v1:"
DEFAULT_MAX_REDEMPTIONS = 1_000_000


def normalize_invite_code(code: str) -> str:
    normalized = "".join(
        char.upper() for char in code.strip() if not char.isspace() and char != "-"
    )
    if not normalized:
        raise ValueError("invite code is empty after normalization")
    return normalized


def hash_invite_code(code: str, pepper: str) -> str:
    normalized = normalize_invite_code(code)
    digest = hmac.new(
        pepper.encode("utf-8"),
        HASH_DOMAIN + normalized.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()
    return digest


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def build_sql(code_hash_hex: str, max_redemptions: int, expires_at: str | None) -> str:
    expires_sql = "NULL" if not expires_at else f"{sql_literal(expires_at)}::timestamptz"
    return f"""\
INSERT INTO alpha_invites (code_hash, max_redemptions, expires_at)
VALUES (decode('{code_hash_hex}', 'hex'), {max_redemptions}, {expires_sql})
ON CONFLICT (code_hash) DO UPDATE
SET max_redemptions = GREATEST(alpha_invites.max_redemptions, EXCLUDED.max_redemptions),
    expires_at = EXCLUDED.expires_at
RETURNING invite_id, max_redemptions, redemption_count, expires_at;
"""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--code",
        default=os.environ.get("ALPHA_INVITE_CODE"),
        help="Raw invite code. Defaults to ALPHA_INVITE_CODE.",
    )
    parser.add_argument(
        "--pepper",
        default=os.environ.get("ALPHA_INVITE_CODE_PEPPER"),
        help="Invite HMAC pepper. Defaults to ALPHA_INVITE_CODE_PEPPER.",
    )
    parser.add_argument(
        "--database-url",
        default=os.environ.get("DATABASE_URL"),
        help="Postgres URL used with --execute. Defaults to DATABASE_URL.",
    )
    parser.add_argument(
        "--max-redemptions",
        type=int,
        default=DEFAULT_MAX_REDEMPTIONS,
        help=f"Maximum redemptions to seed. Defaults to {DEFAULT_MAX_REDEMPTIONS}.",
    )
    parser.add_argument(
        "--expires-at",
        help="Optional timestamptz value, for example 2026-12-31T23:59:59Z.",
    )
    parser.add_argument(
        "--execute",
        action="store_true",
        help="Execute the generated SQL with psql instead of printing it.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not args.code:
        print("error: --code or ALPHA_INVITE_CODE is required", file=sys.stderr)
        return 2
    if not args.pepper:
        print("error: --pepper or ALPHA_INVITE_CODE_PEPPER is required", file=sys.stderr)
        return 2
    if len(args.pepper) < 32:
        print("error: invite pepper must be at least 32 bytes", file=sys.stderr)
        return 2
    if args.max_redemptions <= 0:
        print("error: --max-redemptions must be positive", file=sys.stderr)
        return 2

    try:
        code_hash_hex = hash_invite_code(args.code, args.pepper)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    sql = build_sql(code_hash_hex, args.max_redemptions, args.expires_at)

    if not args.execute:
        print(sql, end="")
        return 0

    if not args.database_url:
        print("error: --database-url or DATABASE_URL is required with --execute", file=sys.stderr)
        return 2

    completed = subprocess.run(
        ["psql", args.database_url, "-v", "ON_ERROR_STOP=1"],
        input=sql,
        text=True,
        check=False,
    )
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
