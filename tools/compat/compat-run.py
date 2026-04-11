#!/usr/bin/env python3
"""Local macOS compatibility runner for Wavis desktop builds."""

from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import datetime as dt
import hashlib
import json
import plistlib
import shlex
import shutil
import subprocess
import sys
import time
try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore[no-redef]  # Python < 3.11
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_LOCAL_CONFIG = REPO_ROOT / "tools" / "compat" / "machines.local.toml"
DEFAULT_EXAMPLE_CONFIG = REPO_ROOT / "tools" / "compat" / "machines.example.toml"
TAURI_CONFIG = REPO_ROOT / "clients" / "wavis-gui" / "src-tauri" / "tauri.conf.json"
AGENT_SCRIPT = REPO_ROOT / "tools" / "compat" / "agent" / "run-agent.sh"
MERGE_SCRIPT = REPO_ROOT / "tools" / "compat" / "report" / "merge-report.py"
REMOTE_BASE = "/tmp/wavis-compat"
SUPPORTED_TIERS = {"t0", "t1", "t2", "t3"}
ENTITLEMENT_KEYS = [
    "com.apple.security.cs.allow-jit",
    "com.apple.security.device.audio-input",
    "com.apple.security.cs.disable-library-validation",
]


@dataclasses.dataclass(frozen=True)
class Machine:
    name: str
    host: str
    user: str
    ssh_key: str
    arch: str = ""
    macos: str = ""
    notes: str = ""
    tiers: tuple[str, ...] = ("t0", "t1")

    @property
    def target(self) -> str:
        return f"{self.user}@{self.host}"

    @property
    def expanded_ssh_key(self) -> str:
        return str(Path(self.ssh_key).expanduser())


@dataclasses.dataclass
class CommandResult:
    returncode: int
    stdout: str
    stderr: str
    timed_out: bool = False


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")


def timestamp() -> str:
    return dt.datetime.now().strftime("%Y%m%d-%H%M%S")


def display_cmd(cmd: list[str]) -> str:
    return shlex.join(str(part) for part in cmd)


def read_tauri_minimum_system_version() -> str:
    try:
        config = json.loads(TAURI_CONFIG.read_text(encoding="utf-8"))
        return config["bundle"]["macOS"]["minimumSystemVersion"]
    except Exception:
        return "10.15"


def parse_tiers(raw: str) -> list[str]:
    tiers = [tier.strip() for tier in raw.split(",") if tier.strip()]
    unknown = sorted(set(tiers) - SUPPORTED_TIERS)
    if unknown:
        raise argparse.ArgumentTypeError(
            f"unsupported tier(s): {', '.join(unknown)}"
        )
    return tiers or ["t0", "t1"]


def select_config(args: argparse.Namespace) -> Path:
    if args.config:
        return Path(args.config).expanduser()
    if DEFAULT_LOCAL_CONFIG.exists():
        return DEFAULT_LOCAL_CONFIG
    if args.dry_run and DEFAULT_EXAMPLE_CONFIG.exists():
        print(
            f"[warn] {DEFAULT_LOCAL_CONFIG.relative_to(REPO_ROOT)} not found; "
            f"dry-run using {DEFAULT_EXAMPLE_CONFIG.relative_to(REPO_ROOT)}"
        )
        return DEFAULT_EXAMPLE_CONFIG
    return DEFAULT_LOCAL_CONFIG


def load_machines(config_path: Path) -> list[Machine]:
    with config_path.open("rb") as f:
        data = tomllib.load(f)

    machines: list[Machine] = []
    for entry in data.get("machines", []):
        missing = [key for key in ("name", "host", "user", "ssh_key") if not entry.get(key)]
        if missing:
            raise ValueError(f"machine entry missing required keys: {', '.join(missing)}")
        machines.append(
            Machine(
                name=str(entry["name"]),
                host=str(entry["host"]),
                user=str(entry["user"]),
                ssh_key=str(entry["ssh_key"]),
                arch=str(entry.get("arch", "")),
                macos=str(entry.get("macos", "")),
                notes=str(entry.get("notes", "")),
                tiers=tuple(str(tier) for tier in entry.get("tiers", ["t0", "t1"])),
            )
        )
    return machines


def find_app_binary(app_path: Path) -> Path:
    info_path = app_path / "Contents" / "Info.plist"
    executable = app_path.stem
    if info_path.exists():
        try:
            with info_path.open("rb") as f:
                plist = plistlib.load(f)
            executable = str(plist.get("CFBundleExecutable") or executable)
        except Exception:
            pass
    return app_path / "Contents" / "MacOS" / executable


def run_local(cmd: list[str], timeout: int | None = None) -> CommandResult:
    try:
        completed = subprocess.run(
            cmd,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
        return CommandResult(completed.returncode, completed.stdout, completed.stderr)
    except subprocess.TimeoutExpired as exc:
        return CommandResult(
            124,
            exc.stdout if isinstance(exc.stdout, str) else "",
            exc.stderr if isinstance(exc.stderr, str) else "",
            timed_out=True,
        )


def write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def compare_versions(left: str, right: str) -> int:
    def parts(value: str) -> list[int]:
        return [int(piece) for piece in value.split(".") if piece.isdigit()]

    a = parts(left)
    b = parts(right)
    length = max(len(a), len(b))
    a.extend([0] * (length - len(a)))
    b.extend([0] * (length - len(b)))
    return (a > b) - (a < b)


def parse_deployment_versions(otool_output: str) -> list[str]:
    versions: list[str] = []
    lines = otool_output.splitlines()
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("minos "):
            parts = stripped.split()
            if len(parts) >= 2:
                versions.append(parts[1])
        elif stripped == "cmd LC_VERSION_MIN_MACOSX":
            for follow in lines[index + 1 : index + 6]:
                follow = follow.strip()
                if follow.startswith("version "):
                    parts = follow.split()
                    if len(parts) >= 2:
                        versions.append(parts[1])
                    break
    return versions


def run_tier0(app_path: Path, output_dir: Path) -> dict[str, Any]:
    expected_deployment = read_tauri_minimum_system_version()
    output_dir.mkdir(parents=True, exist_ok=True)
    binary = find_app_binary(app_path)
    notes: list[str] = []
    failures: list[str] = []
    artifacts: list[str] = []

    app_info = {
        "app_path": str(app_path),
        "binary_path": str(binary),
        "expected_minimum_system_version": expected_deployment,
        "runner_platform": sys.platform,
    }
    write_text(output_dir / "app-info.json", json.dumps(app_info, indent=2) + "\n")
    artifacts.append("app-info.json")

    if not app_path.exists():
        failures.append(f"app path does not exist: {app_path}")
    elif not app_path.name.endswith(".app"):
        failures.append(f"app path is not a .app bundle: {app_path}")
    elif not binary.exists():
        failures.append(f"bundle executable not found: {binary}")

    if failures:
        result = {"pass": False, "notes": notes, "failures": failures, "artifacts": artifacts}
        write_text(output_dir / "result.json", json.dumps({"tiers": {"t0": result}}, indent=2) + "\n")
        return result

    if shutil.which("otool"):
        otool_l = run_local(["otool", "-l", str(binary)])
        write_text(output_dir / "otool-l.txt", otool_l.stdout + otool_l.stderr)
        artifacts.append("otool-l.txt")
        if otool_l.returncode != 0:
            failures.append(f"otool -l failed with exit code {otool_l.returncode}")
        else:
            deployment_versions = parse_deployment_versions(otool_l.stdout)
            if not deployment_versions:
                failures.append("otool -l did not include an LC_BUILD_VERSION deployment target")
            for version in deployment_versions:
                if compare_versions(version, expected_deployment) != 0:
                    failures.append(
                        "deployment target mismatch: "
                        f"binary reports {version}, tauri.conf.json expects {expected_deployment}"
                    )

        otool_libs = run_local(["otool", "-L", str(binary)])
        write_text(output_dir / "otool-L.txt", otool_libs.stdout + otool_libs.stderr)
        artifacts.append("otool-L.txt")
        if otool_libs.returncode != 0:
            failures.append(f"otool -L failed with exit code {otool_libs.returncode}")
    else:
        notes.append("otool not found; skipped deployment target and dylib scans")

    if shutil.which("codesign"):
        verify = run_local(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app_path)])
        write_text(output_dir / "codesign-verify.txt", verify.stdout + verify.stderr)
        artifacts.append("codesign-verify.txt")
        if verify.returncode != 0:
            notes.append(f"codesign verification failed with exit code {verify.returncode}")

        entitlements = run_local(["codesign", "--display", "--entitlements", ":-", str(app_path)])
        entitlement_text = entitlements.stdout + entitlements.stderr
        write_text(output_dir / "entitlements.xml", entitlement_text)
        artifacts.append("entitlements.xml")
        if entitlements.returncode != 0:
            notes.append(f"codesign entitlement display failed with exit code {entitlements.returncode}")
        else:
            for key in ENTITLEMENT_KEYS:
                if key not in entitlement_text:
                    failures.append(f"missing entitlement: {key}")
    else:
        notes.append("codesign not found; skipped signature and entitlement checks")

    result = {"pass": not failures, "notes": notes, "failures": failures, "artifacts": artifacts}
    write_text(output_dir / "result.json", json.dumps({"tiers": {"t0": result}}, indent=2) + "\n")
    return result


def ssh_options(machine: Machine) -> list[str]:
    return [
        "-i",
        machine.expanded_ssh_key,
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=3",
    ]


def ssh_cmd(machine: Machine, remote_cmd: str) -> list[str]:
    return ["ssh", *ssh_options(machine), machine.target, remote_cmd]


def scp_cmd(machine: Machine, source: str, destination: str) -> list[str]:
    return ["scp", "-r", *ssh_options(machine), source, destination]


def run_with_retry(cmd: list[str], timeout_secs: int, dry_run: bool, retries: int = 1) -> CommandResult:
    if dry_run:
        print(f"[dry-run] {display_cmd(cmd)}")
        return CommandResult(0, "", "")

    last = CommandResult(1, "", "command did not run")
    for attempt in range(retries + 1):
        last = run_local(cmd, timeout=timeout_secs)
        if last.returncode == 0 and not last.timed_out:
            return last
        if attempt < retries:
            time.sleep(5)
    return last


def machine_result(
    machine: Machine,
    status: str,
    reason: str,
    tiers: list[str],
    local_dir: Path,
) -> dict[str, Any]:
    result = {
        "machine": dataclasses.asdict(machine),
        "status": status,
        "generated_at": utc_now(),
        "tiers": {
            tier: {"pass": False, "notes": [reason], "failures": [reason]}
            for tier in tiers
            if tier != "t0"
        },
    }
    write_text(local_dir / "result.json", json.dumps(result, indent=2) + "\n")
    return result


def run_remote_machine(
    machine: Machine,
    app_path: Path,
    run_id: str,
    requested_tiers: list[str],
    timeout_secs: int,
    results_dir: Path,
    dry_run: bool,
) -> dict[str, Any]:
    remote_tiers = [tier for tier in requested_tiers if tier != "t0" and tier in machine.tiers]
    local_dir = results_dir / machine.name
    local_dir.mkdir(parents=True, exist_ok=True)
    if not remote_tiers:
        return machine_result(
            machine,
            "skipped",
            "no requested remote tiers apply to this machine",
            requested_tiers,
            local_dir,
        )

    remote_root = f"{REMOTE_BASE}/{run_id}-{machine.name}"
    remote_app = f"{remote_root}/{app_path.name}"
    remote_out = f"{remote_root}/out"
    remote_agent = f"{remote_root}/run-agent.sh"

    setup = run_with_retry(
        ssh_cmd(machine, f"mkdir -p {shlex.quote(remote_root)} {shlex.quote(remote_out)}"),
        timeout_secs,
        dry_run,
    )
    if setup.returncode != 0:
        reason = setup.stderr.strip() or setup.stdout.strip() or "SSH setup failed"
        return machine_result(machine, "unreachable", reason, remote_tiers, local_dir)

    upload_app = run_with_retry(
        scp_cmd(machine, str(app_path), f"{machine.target}:{remote_root}/"),
        timeout_secs,
        dry_run,
    )
    if upload_app.returncode != 0:
        reason = upload_app.stderr.strip() or upload_app.stdout.strip() or "SCP app upload failed"
        return machine_result(machine, "unreachable", reason, remote_tiers, local_dir)

    upload_agent = run_with_retry(
        scp_cmd(machine, str(AGENT_SCRIPT), f"{machine.target}:{remote_agent}"),
        timeout_secs,
        dry_run,
    )
    if upload_agent.returncode != 0:
        reason = upload_agent.stderr.strip() or upload_agent.stdout.strip() or "SCP agent upload failed"
        return machine_result(machine, "unreachable", reason, remote_tiers, local_dir)

    agent_args = [
        "bash",
        remote_agent,
        "--app",
        remote_app,
        "--out",
        remote_out,
        "--machine",
        machine.name,
        "--tiers",
        ",".join(remote_tiers),
        "--timeout",
        str(timeout_secs),
    ]
    run_agent = run_with_retry(
        ssh_cmd(machine, " ".join(shlex.quote(arg) for arg in agent_args)),
        timeout_secs,
        dry_run,
        retries=0,
    )
    write_text(local_dir / "agent-stdout.txt", run_agent.stdout)
    write_text(local_dir / "agent-stderr.txt", run_agent.stderr)
    if run_agent.timed_out:
        return machine_result(
            machine,
            "timeout",
            f"remote agent timed out after {timeout_secs}s",
            remote_tiers,
            local_dir,
        )

    fetch = run_with_retry(
        scp_cmd(machine, f"{machine.target}:{remote_out}/.", str(local_dir)),
        timeout_secs,
        dry_run,
        retries=0,
    )
    if fetch.returncode != 0:
        reason = fetch.stderr.strip() or fetch.stdout.strip() or "SCP result fetch failed"
        return machine_result(machine, "failed", reason, remote_tiers, local_dir)

    if dry_run:
        result = {
            "machine": dataclasses.asdict(machine),
            "status": "dry-run",
            "generated_at": utc_now(),
            "tiers": {tier: {"pass": True, "notes": ["dry run only"]} for tier in remote_tiers},
        }
        write_text(local_dir / "result.json", json.dumps(result, indent=2) + "\n")
        return result

    result_path = local_dir / "result.json"
    if not result_path.exists():
        return machine_result(machine, "failed", "remote agent did not produce result.json", remote_tiers, local_dir)
    try:
        return json.loads(result_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return machine_result(machine, "failed", f"remote result.json is invalid JSON: {exc}", remote_tiers, local_dir)


def file_sha256(path: Path) -> str:
    if not path.is_file():
        return ""
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def app_fingerprint(app_path: Path) -> str:
    return file_sha256(find_app_binary(app_path))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run Wavis macOS compatibility smoke tests.")
    parser.add_argument("--app", required=True, help="Path to the built Wavis.app bundle.")
    parser.add_argument("--config", help="Machine inventory TOML. Defaults to tools/compat/machines.local.toml.")
    parser.add_argument("--machine", action="append", default=[], help="Machine name to run. Repeatable.")
    parser.add_argument("--tiers", type=parse_tiers, default=["t0", "t1"], help="Comma-separated tiers. Supports t0,t1,t2,t3.")
    parser.add_argument("--parallel", type=int, default=1, help="Remote machine parallelism, 1-4. Default: 1.")
    parser.add_argument("--timeout", type=int, default=120, help="Remote SSH/SCP timeout in seconds. Default: 120.")
    parser.add_argument("--results-dir", default="compat-results", help="Directory for result bundles.")
    parser.add_argument("--dry-run", action="store_true", help="Print SSH/SCP commands without connecting.")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.parallel < 1 or args.parallel > 4:
        print("--parallel must be between 1 and 4", file=sys.stderr)
        return 2

    app_path = Path(args.app).expanduser().resolve()
    requested_tiers = args.tiers
    if not args.dry_run and not app_path.exists():
        print(f"app path does not exist: {app_path}", file=sys.stderr)
        return 2

    run_id = timestamp()
    results_dir = (Path(args.results_dir).expanduser() / run_id).resolve()
    results_dir.mkdir(parents=True, exist_ok=True)

    print("=== Wavis macOS compatibility run ===")
    print(f"app        : {app_path}")
    print(f"tiers      : {','.join(requested_tiers)}")
    print(f"results    : {results_dir}")
    print(f"parallel   : {args.parallel}")

    if "t0" in requested_tiers:
        print("\n--- Tier 0: local package validation ---")
        t0_result = run_tier0(app_path, results_dir / "_local")
        state = "PASS" if t0_result["pass"] else "FAIL"
        print(f"T0 {state}")
        for note in t0_result["notes"]:
            print(f"  [warn] {note}")
        for failure in t0_result["failures"]:
            print(f"  [fail] {failure}")

    remote_requested = [tier for tier in requested_tiers if tier != "t0"]
    if remote_requested:
        config_path = select_config(args)
        if not config_path.exists():
            print(
                f"machine config not found: {config_path}\n"
                "Create tools/compat/machines.local.toml from machines.example.toml "
                "or pass --config.",
                file=sys.stderr,
            )
            return 2
        machines = load_machines(config_path)
        if args.machine:
            wanted = set(args.machine)
            machines = [machine for machine in machines if machine.name in wanted]
            missing = wanted - {machine.name for machine in machines}
            if missing:
                print(f"unknown machine(s): {', '.join(sorted(missing))}", file=sys.stderr)
                return 2

        print("\n--- Tier 1: remote launch checks ---")
        print(f"config     : {config_path}")
        print(f"machines   : {', '.join(machine.name for machine in machines) or '(none)'}")
        if args.parallel > 1:
            print("[warn] parallel app upload can saturate upstream bandwidth")

        with concurrent.futures.ThreadPoolExecutor(max_workers=args.parallel) as pool:
            future_to_machine = {
                pool.submit(
                    run_remote_machine,
                    machine,
                    app_path,
                    run_id,
                    requested_tiers,
                    args.timeout,
                    results_dir,
                    args.dry_run,
                ): machine
                for machine in machines
            }
            for future in concurrent.futures.as_completed(future_to_machine):
                machine = future_to_machine[future]
                try:
                    result = future.result()
                except Exception as exc:
                    result = machine_result(machine, "failed", str(exc), requested_tiers, results_dir / machine.name)
                status = result.get("status", "unknown")
                tier_states = []
                for tier, tier_result in result.get("tiers", {}).items():
                    tier_states.append(f"{tier}={'PASS' if tier_result.get('pass') else 'FAIL'}")
                print(f"{machine.name}: {status} {' '.join(tier_states)}")

    merge_cmd = [
        sys.executable,
        str(MERGE_SCRIPT),
        "--results-dir",
        str(results_dir),
        "--app",
        str(app_path),
        "--app-sha",
        app_fingerprint(app_path),
    ]
    print("\n--- Report ---")
    merged = run_local(merge_cmd)
    sys.stdout.write(merged.stdout)
    sys.stderr.write(merged.stderr)
    if merged.returncode != 0:
        return merged.returncode
    if args.dry_run:
        return 0

    try:
        report = json.loads((results_dir / "compat-report.json").read_text(encoding="utf-8"))
    except Exception:
        return 1

    local_failed = any(
        not tier_result.get("pass")
        for tier_result in (report.get("local") or {}).get("tiers", {}).values()
    )
    remote_failed = bool(report.get("summary", {}).get("failed"))
    return 1 if local_failed or remote_failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
