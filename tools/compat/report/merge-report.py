#!/usr/bin/env python3
"""Merge Wavis compatibility result bundles into JSON and Markdown summaries."""

from __future__ import annotations

import argparse
import datetime as dt
import json
from pathlib import Path
from typing import Any


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")


def load_json(path: Path) -> dict[str, Any] | None:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return None


def collect_results(results_dir: Path) -> tuple[dict[str, Any] | None, list[dict[str, Any]]]:
    local = load_json(results_dir / "_local" / "result.json")
    machines: list[dict[str, Any]] = []
    for child in sorted(results_dir.iterdir()):
        if not child.is_dir() or child.name == "_local":
            continue
        result = load_json(child / "result.json")
        if result is None:
            result = {"machine": {"name": child.name}, "status": "missing-result", "tiers": {}}
        result["log_bundle"] = str(child)
        machines.append(result)
    return local, machines


def tier_passes(result: dict[str, Any]) -> bool:
    tiers = result.get("tiers", {})
    return bool(tiers) and all(bool(tier.get("pass")) for tier in tiers.values())


def first_failure_note(tier_result: dict[str, Any]) -> str:
    failures = tier_result.get("failures") or []
    notes = tier_result.get("notes") or []
    if failures:
        return str(failures[0])
    if notes:
        return str(notes[0])
    return ""


def build_report(results_dir: Path, app: str, app_sha: str) -> dict[str, Any]:
    local, machines = collect_results(results_dir)
    failed = [
        machine.get("machine", {}).get("name", "unknown")
        for machine in machines
        if not tier_passes(machine)
    ]
    return {
        "schema_version": 1,
        "generated_at": utc_now(),
        "app": app,
        "app_sha": app_sha,
        "results_dir": str(results_dir),
        "local": local,
        "machines": machines,
        "summary": {
            "total": len(machines),
            "passed_all_tiers": len(machines) - len(failed),
            "failed": len(failed),
            "failure_machines": failed,
        },
    }


def markdown_summary(report: dict[str, Any]) -> str:
    summary = report["summary"]
    lines = [
        "# Wavis macOS Compatibility Report",
        "",
        f"- Generated: {report['generated_at']}",
        f"- App: {report.get('app') or '(unknown)'}",
        f"- App SHA: {report.get('app_sha') or '(directory artifact)'}",
        f"- Results: {report.get('results_dir')}",
        "",
        "## Summary",
        "",
        f"- Machines: {summary['total']} total, {summary['passed_all_tiers']} passed, {summary['failed']} failed",
    ]

    local = report.get("local")
    if local and local.get("tiers"):
        lines.extend(["", "## Local Tiers", ""])
        for tier, tier_result in local["tiers"].items():
            state = "PASS" if tier_result.get("pass") else "FAIL"
            note = first_failure_note(tier_result)
            lines.append(f"- {tier}: {state}{' - ' + note if note else ''}")

    lines.extend(["", "## Machines", ""])
    if not report["machines"]:
        lines.append("- No remote machines were run.")
    for machine in report["machines"]:
        name = machine.get("machine", {}).get("name") or "unknown"
        status = machine.get("status", "unknown")
        lines.append(f"- {name}: {status}")
        tiers = machine.get("tiers") or {}
        if not tiers:
            lines.append("  - no tier results")
        for tier, tier_result in tiers.items():
            state = "PASS" if tier_result.get("pass") else "FAIL"
            note = first_failure_note(tier_result)
            lines.append(f"  - {tier}: {state}{' - ' + note if note else ''}")
    lines.append("")
    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Merge Wavis compatibility result bundles.")
    parser.add_argument("--results-dir", required=True)
    parser.add_argument("--app", default="")
    parser.add_argument("--app-sha", default="")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    results_dir = Path(args.results_dir).expanduser().resolve()
    report = build_report(results_dir, args.app, args.app_sha)
    report_json = results_dir / "compat-report.json"
    report_md = results_dir / "compat-report.md"
    report_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    report_md.write_text(markdown_summary(report), encoding="utf-8")

    summary = report["summary"]
    print(f"Wrote {report_json}")
    print(f"Wrote {report_md}")
    print(f"Summary: {summary['passed_all_tiers']}/{summary['total']} machines passed all requested tiers")
    if summary["failure_machines"]:
        print("Failures: " + ", ".join(summary["failure_machines"]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
