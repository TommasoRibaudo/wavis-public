#!/usr/bin/env python3
"""Merge Wavis compatibility result bundles into JSON and Markdown summaries."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from pathlib import Path
from typing import Any

CODE_PREFIX_RE = re.compile(r"^([A-Z][A-Z0-9_]+):\s*(.*)$")
TIER_ORDER = ("t0", "t1", "t2", "t3")
FAILURE_CATEGORIES = {
    "AGENT_TIMEOUT": "remote_runner",
    "APP_BUNDLE_INVALID": "bundle_layout",
    "APP_BUNDLE_MISSING": "bundle_layout",
    "ARCH_MISMATCH": "binary_architecture",
    "ARCH_UNKNOWN": "binary_architecture",
    "AUDIO_DEVICES_EMPTY": "audio_device_enumeration",
    "BINARY_MISSING": "bundle_layout",
    "CODESIGN_INVALID": "code_signing",
    "DEPLOYMENT_TARGET_MISMATCH": "deployment_target",
    "DEPLOYMENT_TARGET_MISSING": "deployment_target",
    "ENTITLEMENT_MISSING": "code_signing",
    "IPC_FAILED": "ipc_bridge",
    "IPC_TIMEOUT": "ipc_bridge",
    "LAUNCH_CRASH": "launch",
    "LAUNCH_NOT_RUNNING": "launch",
    "LAUNCH_OPEN_FAILED": "launch",
    "LIPO_FAILED": "binary_architecture",
    "NO_REMOTE_TIERS": "runner_configuration",
    "NOTARIZATION_MISSING": "notarization",
    "OTOOL_DYLIBS_FAILED": "static_analysis",
    "OTOOL_LOAD_COMMANDS_FAILED": "static_analysis",
    "PLIST_VERSION_MISMATCH": "deployment_target",
    "REMOTE_RESULT_INVALID": "remote_runner",
    "REMOTE_RESULT_MISSING": "remote_runner",
    "REMOTE_RUN_FAILED": "remote_runner",
    "ROSETTA_DETECTED": "binary_architecture",
    "SCK_HARD_LINKED": "screencapturekit_compatibility",
    "SCK_VERSION_WRONG": "screencapturekit_compatibility",
    "SCP_AGENT_UPLOAD_FAILED": "remote_runner",
    "SCP_APP_UPLOAD_FAILED": "remote_runner",
    "SCP_RESULT_FETCH_FAILED": "remote_runner",
    "SSH_SETUP_FAILED": "remote_runner",
    "STORE_FAILED": "plugin_store",
    "TAP_VERSION_WRONG": "process_tap_compatibility",
    "TCC_DENIED": "tcc_permissions",
}


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
        enrich_machine_info(result, child)
        result["log_bundle"] = str(child)
        machines.append(result)
    return local, machines


def proc_translated_is_active(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, int):
        return value == 1
    return str(value).strip() == "1"


def enrich_machine_info(result: dict[str, Any], result_dir: Path) -> None:
    machine = result.get("machine")
    if not isinstance(machine, dict):
        machine = {"name": str(machine or result_dir.name)}
        result["machine"] = machine

    machine_info = load_json(result_dir / "machine-info.json")
    if not isinstance(machine_info, dict):
        machine.setdefault("rosetta_active", None)
        return

    if "proc_translated" in machine_info:
        machine["rosetta_active"] = proc_translated_is_active(machine_info.get("proc_translated"))
    else:
        machine.setdefault("rosetta_active", None)
    if machine_info.get("macos"):
        machine.setdefault("macos_version", machine_info["macos"])
    if machine_info.get("arch"):
        machine.setdefault("hardware_arch", machine_info["arch"])
    if machine_info.get("model"):
        machine.setdefault("model", machine_info["model"])


def tier_passes(result: dict[str, Any]) -> bool:
    tiers = result.get("tiers", {})
    return bool(tiers) and all(bool(tier.get("pass")) for tier in tiers.values())


def parse_issue(issue: Any) -> tuple[str, str]:
    if isinstance(issue, dict):
        code = str(issue.get("code") or "").strip()
        message = str(issue.get("message") or "").strip()
        return code, message

    text = str(issue).strip()
    match = CODE_PREFIX_RE.match(text)
    if match:
        return match.group(1), match.group(2).strip()
    return "", text


def format_issue(issue: Any) -> str:
    code, message = parse_issue(issue)
    if code and message:
        return f"{code}: {message}"
    if message:
        return message
    if code:
        return code
    return str(issue)


def first_failure_note(tier_result: dict[str, Any]) -> str:
    failures = tier_result.get("failures") or []
    notes = tier_result.get("notes") or []
    if failures:
        return format_issue(failures[0])
    if notes:
        return format_issue(notes[0])
    return ""


def first_failure_code(tier_result: dict[str, Any]) -> str:
    for issue in tier_result.get("failures") or []:
        code, _ = parse_issue(issue)
        if code:
            return code
    return ""


def likely_failure_category(code: str) -> str | None:
    if not code:
        return None
    return FAILURE_CATEGORIES.get(code, "unknown")


def first_failing_phase_and_category(result: dict[str, Any]) -> tuple[str | None, str | None]:
    tiers = result.get("tiers") or {}
    for tier in TIER_ORDER:
        tier_result = tiers.get(tier)
        if isinstance(tier_result, dict) and not bool(tier_result.get("pass")):
            code = first_failure_code(tier_result)
            return tier, likely_failure_category(code) or "unstructured_failure"
    for tier, tier_result in sorted(tiers.items()):
        if isinstance(tier_result, dict) and not bool(tier_result.get("pass")):
            code = first_failure_code(tier_result)
            return str(tier), likely_failure_category(code) or "unstructured_failure"
    return None, None


def t0_result(local: dict[str, Any] | None) -> dict[str, Any]:
    if not isinstance(local, dict):
        return {}
    tiers = local.get("tiers")
    if not isinstance(tiers, dict):
        return {}
    t0 = tiers.get("t0")
    return t0 if isinstance(t0, dict) else {}


def build_app_summary(app_path: str, app_sha: str, local: dict[str, Any] | None) -> dict[str, Any]:
    t0 = t0_result(local)
    return {
        "path": app_path,
        "binary_sha256": app_sha,
        "binary_arch": t0.get("binary_arch"),
        "plist_min_version": t0.get("plist_min_version"),
        "sck_link_type": t0.get("sck_link_type"),
        "notarization_stapled": t0.get("notarization_stapled"),
        "version": t0.get("app_version"),
        "build_id": t0.get("app_build_id"),
    }


def build_report(results_dir: Path, app: str, app_sha: str) -> dict[str, Any]:
    local, machines = collect_results(results_dir)
    for machine in machines:
        phase, category = first_failing_phase_and_category(machine)
        machine["first_failing_phase"] = phase
        machine["likely_failure_category"] = category

    failed = [
        machine.get("machine", {}).get("name", "unknown")
        for machine in machines
        if not tier_passes(machine)
    ]
    return {
        "schema_version": 1,
        "generated_at": utc_now(),
        "app": build_app_summary(app, app_sha, local),
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
    app = report.get("app")
    app_path = app.get("path") if isinstance(app, dict) else app
    app_sha = app.get("binary_sha256") if isinstance(app, dict) else report.get("app_sha")
    lines = [
        "# Wavis macOS Compatibility Report",
        "",
        f"- Generated: {report['generated_at']}",
        f"- App: {app_path or '(unknown)'}",
        f"- App SHA: {app_sha or '(directory artifact)'}",
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
        lines.append(f"  - first failing phase: {machine.get('first_failing_phase') or 'none'}")
        lines.append(f"  - likely category: {machine.get('likely_failure_category') or 'none'}")
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
