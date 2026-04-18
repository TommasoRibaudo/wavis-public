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
import re
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


def parse_lipo_arches(lipo_output: str) -> str:
    for line in lipo_output.splitlines():
        fat_match = re.search(r"\bare:\s*(.+)$", line)
        if fat_match:
            return " ".join(fat_match.group(1).split())
        non_fat_match = re.search(r"\barchitecture:\s*(.+)$", line)
        if non_fat_match:
            return " ".join(non_fat_match.group(1).split())
    return ""


def parse_sck_link_type(otool_output: str) -> str:
    found_weak = False
    found_strong = False
    current_cmd: str | None = None
    block_mentions_sck = False

    def finish_block() -> None:
        nonlocal found_weak, found_strong, current_cmd, block_mentions_sck
        if block_mentions_sck:
            if current_cmd == "LC_LOAD_DYLIB":
                found_strong = True
            elif current_cmd == "LC_LOAD_WEAK_DYLIB":
                found_weak = True
        current_cmd = None
        block_mentions_sck = False

    for line in otool_output.splitlines():
        stripped = line.strip()
        if stripped.startswith("Load command "):
            finish_block()
            continue
        if stripped.startswith("cmd "):
            current_cmd = stripped.split(None, 1)[1].split()[0]
        if "ScreenCaptureKit" in stripped:
            block_mentions_sck = True
    finish_block()

    if found_strong:
        return "strong"
    if found_weak:
        return "weak"
    return "absent"


def normalize_arch(arch: str) -> str:
    normalized = arch.strip().lower()
    aliases = {
        "amd64": "x86_64",
        "x64": "x86_64",
        "aarch64": "arm64",
    }
    return aliases.get(normalized, normalized)


def failure_with_code(code: str, message: str) -> dict[str, str]:
    return {"code": code, "message": message}


def load_app_info_plist(app_path: Path) -> dict[str, Any]:
    info_path = app_path / "Contents" / "Info.plist"
    with info_path.open("rb") as f:
        data = plistlib.load(f)
    return data if isinstance(data, dict) else {}


def format_failure(failure: Any) -> str:
    if isinstance(failure, dict):
        code = str(failure.get("code") or "").strip()
        message = str(failure.get("message") or "").strip()
        if code and message:
            return f"{code}: {message}"
        if message:
            return message
        if code:
            return code
    return str(failure)


def run_tier0(
    app_path: Path,
    output_dir: Path,
    machines: list[Machine] | None = None,
    debug: bool = False,
) -> dict[str, Any]:
    expected_deployment = read_tauri_minimum_system_version()
    output_dir.mkdir(parents=True, exist_ok=True)
    binary = find_app_binary(app_path)
    notes: list[str] = []
    failures: list[Any] = []
    artifacts: list[str] = []
    binary_arch = ""
    arch_matches_expected = True
    notarization_stapled: bool | None = None
    sck_link_type: str | None = None
    plist_min_version: str | None = None
    plist_min_version_match: bool | None = None
    app_version: str | None = None
    app_build_id: str | None = None

    app_info = {
        "app_path": str(app_path),
        "binary_path": str(binary),
        "expected_minimum_system_version": expected_deployment,
        "runner_platform": sys.platform,
        "debug_build": debug,
    }
    write_text(output_dir / "app-info.json", json.dumps(app_info, indent=2) + "\n")
    artifacts.append("app-info.json")

    if not app_path.exists():
        failures.append(failure_with_code("APP_BUNDLE_MISSING", f"app path does not exist: {app_path}"))
    elif not app_path.name.endswith(".app"):
        failures.append(failure_with_code("APP_BUNDLE_INVALID", f"app path is not a .app bundle: {app_path}"))
    elif not binary.exists():
        failures.append(failure_with_code("BINARY_MISSING", f"bundle executable not found: {binary}"))

    if failures:
        result = {"pass": False, "notes": notes, "failures": failures, "artifacts": artifacts}
        write_text(output_dir / "result.json", json.dumps({"tiers": {"t0": result}}, indent=2) + "\n")
        return result

    try:
        info_plist = load_app_info_plist(app_path)
    except Exception as exc:
        plist_min_version_match = False
        failures.append(
            failure_with_code(
                "PLIST_VERSION_MISMATCH",
                f"could not read Contents/Info.plist: {exc}",
            )
        )
    else:
        plist_min_version_value = info_plist.get("LSMinimumSystemVersion")
        plist_min_version = str(plist_min_version_value) if plist_min_version_value is not None else ""
        app_version_value = info_plist.get("CFBundleShortVersionString")
        app_version = str(app_version_value) if app_version_value is not None else None
        app_build_id_value = info_plist.get("CFBundleVersion")
        app_build_id = str(app_build_id_value) if app_build_id_value is not None else None

        if not plist_min_version:
            plist_min_version_match = False
            failures.append(
                failure_with_code(
                    "PLIST_VERSION_MISMATCH",
                    "Info.plist is missing LSMinimumSystemVersion; "
                    f"tauri.conf.json expects {expected_deployment}",
                )
            )
        else:
            plist_min_version_match = compare_versions(plist_min_version, expected_deployment) == 0
            if not plist_min_version_match:
                failures.append(
                    failure_with_code(
                        "PLIST_VERSION_MISMATCH",
                        "Info.plist minimum system version mismatch: "
                        f"LSMinimumSystemVersion is {plist_min_version}, "
                        f"tauri.conf.json expects {expected_deployment}",
                    )
                )

    if shutil.which("lipo"):
        lipo = run_local(["lipo", "-info", str(binary)])
        lipo_output = lipo.stdout + lipo.stderr
        write_text(output_dir / "lipo-info.txt", lipo_output)
        artifacts.append("lipo-info.txt")
        if lipo.returncode != 0:
            failures.append(failure_with_code("LIPO_FAILED", f"lipo -info failed with exit code {lipo.returncode}"))
        else:
            binary_arch = parse_lipo_arches(lipo_output)
            if not binary_arch:
                failures.append(failure_with_code("ARCH_UNKNOWN", "lipo -info did not report a binary architecture"))
            elif machines is None:
                notes.append("machine inventory unavailable; skipped binary architecture cross-check")
            elif not machines:
                notes.append("machine inventory has no selected machines; skipped binary architecture cross-check")
            else:
                binary_arches = {normalize_arch(arch) for arch in binary_arch.split()}
                for machine in machines:
                    expected_arch = normalize_arch(machine.arch)
                    if not expected_arch:
                        notes.append(f"machine {machine.name} has no arch field; skipped binary architecture cross-check")
                        continue
                    if expected_arch not in binary_arches:
                        arch_matches_expected = False
                        failures.append(
                            failure_with_code(
                                "ARCH_MISMATCH",
                                "binary architecture mismatch: "
                                f"{machine.name} expects {expected_arch}, lipo reports {binary_arch}",
                            )
                        )
    else:
        notes.append("lipo not found; skipped binary architecture check")

    if shutil.which("otool"):
        otool_l = run_local(["otool", "-l", str(binary)])
        otool_l_output = otool_l.stdout + otool_l.stderr
        write_text(output_dir / "otool-l.txt", otool_l_output)
        artifacts.append("otool-l.txt")
        if otool_l.returncode != 0:
            failures.append(failure_with_code("OTOOL_LOAD_COMMANDS_FAILED", f"otool -l failed with exit code {otool_l.returncode}"))
        else:
            deployment_versions = parse_deployment_versions(otool_l.stdout)
            if not deployment_versions:
                failures.append(
                    failure_with_code(
                        "DEPLOYMENT_TARGET_MISSING",
                        "otool -l did not include an LC_BUILD_VERSION deployment target",
                    )
                )
            for version in deployment_versions:
                if compare_versions(version, expected_deployment) != 0:
                    failures.append(
                        failure_with_code(
                            "DEPLOYMENT_TARGET_MISMATCH",
                            "deployment target mismatch: "
                            f"binary reports {version}, tauri.conf.json expects {expected_deployment}",
                        )
                    )
            sck_link_type = parse_sck_link_type(otool_l_output)
            if sck_link_type == "strong":
                failures.append(
                    failure_with_code(
                        "SCK_HARD_LINKED",
                        "ScreenCaptureKit is linked with LC_LOAD_DYLIB; expected LC_LOAD_WEAK_DYLIB or absent",
                    )
                )

        otool_libs = run_local(["otool", "-L", str(binary)])
        write_text(output_dir / "otool-L.txt", otool_libs.stdout + otool_libs.stderr)
        artifacts.append("otool-L.txt")
        if otool_libs.returncode != 0:
            failures.append(failure_with_code("OTOOL_DYLIBS_FAILED", f"otool -L failed with exit code {otool_libs.returncode}"))
    else:
        notes.append("otool not found; skipped deployment target and dylib scans")

    if debug:
        notes.append("skipped: --debug")
    elif shutil.which("xcrun"):
        stapler = run_local(["xcrun", "stapler", "validate", str(app_path)])
        stapler_output = stapler.stdout + stapler.stderr
        write_text(output_dir / "stapler-validate.txt", stapler_output)
        artifacts.append("stapler-validate.txt")
        notarization_stapled = stapler.returncode == 0
        if not notarization_stapled:
            failures.append(
                failure_with_code(
                    "NOTARIZATION_MISSING",
                    f"xcrun stapler validate failed with exit code {stapler.returncode}",
                )
            )
    else:
        notes.append("xcrun not found; skipped notarization staple check")

    if shutil.which("codesign"):
        verify = run_local(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app_path)])
        write_text(output_dir / "codesign-verify.txt", verify.stdout + verify.stderr)
        artifacts.append("codesign-verify.txt")
        if verify.returncode != 0:
            message = f"codesign verification failed with exit code {verify.returncode}"
            if debug:
                notes.append(message)
            else:
                failures.append(failure_with_code("CODESIGN_INVALID", message))

        entitlements = run_local(["codesign", "--display", "--entitlements", ":-", str(app_path)])
        entitlement_text = entitlements.stdout + entitlements.stderr
        write_text(output_dir / "entitlements.xml", entitlement_text)
        artifacts.append("entitlements.xml")
        if entitlements.returncode != 0:
            notes.append(f"codesign entitlement display failed with exit code {entitlements.returncode}")
        else:
            for key in ENTITLEMENT_KEYS:
                if key not in entitlement_text:
                    failures.append(failure_with_code("ENTITLEMENT_MISSING", f"missing entitlement: {key}"))
    else:
        notes.append("codesign not found; skipped signature and entitlement checks")

    result = {
        "pass": not failures,
        "binary_arch": binary_arch,
        "arch_matches_expected": arch_matches_expected,
        "notarization_stapled": notarization_stapled,
        "sck_link_type": sck_link_type,
        "plist_min_version": plist_min_version,
        "plist_min_version_match": plist_min_version_match,
        "app_version": app_version,
        "app_build_id": app_build_id,
        "notes": notes,
        "failures": failures,
        "artifacts": artifacts,
    }
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
    code: str = "REMOTE_RUN_FAILED",
) -> dict[str, Any]:
    failure = failure_with_code(code, reason)
    result = {
        "machine": dataclasses.asdict(machine),
        "status": status,
        "generated_at": utc_now(),
        "tiers": {
            tier: {"pass": False, "notes": [reason], "failures": [failure]}
            for tier in tiers
            if tier != "t0"
        },
    }
    write_text(local_dir / "result.json", json.dumps(result, indent=2) + "\n")
    return result


def load_machine_inventory_for_run(
    args: argparse.Namespace,
    *,
    required: bool,
) -> tuple[Path, list[Machine], str | None]:
    config_path = select_config(args)
    if not config_path.exists():
        if required:
            message = (
                f"machine config not found: {config_path}\n"
                "Create tools/compat/machines.local.toml from machines.example.toml "
                "or pass --config."
            )
        else:
            message = f"machine config not found: {config_path}"
        return config_path, [], message

    machines = load_machines(config_path)
    if args.machine:
        wanted = set(args.machine)
        machines = [machine for machine in machines if machine.name in wanted]
        missing = wanted - {machine.name for machine in machines}
        if missing:
            return config_path, machines, f"unknown machine(s): {', '.join(sorted(missing))}"
    return config_path, machines, None


def read_machine_info(local_dir: Path) -> dict[str, Any]:
    info_path = local_dir / "machine-info.json"
    if not info_path.exists():
        return {}
    try:
        data = json.loads(info_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}
    return data if isinstance(data, dict) else {}


def proc_translated_is_active(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, int):
        return value == 1
    return str(value).strip() == "1"


def apply_rosetta_gate(result: dict[str, Any], machine: Machine, local_dir: Path, remote_tiers: list[str]) -> None:
    if normalize_arch(machine.arch) != "x86_64":
        return
    machine_info = read_machine_info(local_dir)
    if not proc_translated_is_active(machine_info.get("proc_translated", 0)):
        return

    rosetta_failure = failure_with_code(
        "ROSETTA_DETECTED",
        f"{machine.name} is configured as x86_64 but sysctl.proc_translated reported 1",
    )
    tiers = result.setdefault("tiers", {})
    for tier in remote_tiers:
        tier_result = tiers.setdefault(tier, {})
        tier_result["pass"] = False
        failures = tier_result.setdefault("failures", [])
        failures.append(rosetta_failure)
        notes = tier_result.setdefault("notes", [])
        notes.append("remote run invalidated by Rosetta translation")


def run_remote_machine(
    machine: Machine,
    app_path: Path,
    run_id: str,
    requested_tiers: list[str],
    timeout_secs: int,
    results_dir: Path,
    dry_run: bool,
    debug: bool,
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
            "NO_REMOTE_TIERS",
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
        return machine_result(machine, "unreachable", reason, remote_tiers, local_dir, "SSH_SETUP_FAILED")

    upload_app = run_with_retry(
        scp_cmd(machine, str(app_path), f"{machine.target}:{remote_root}/"),
        timeout_secs,
        dry_run,
    )
    if upload_app.returncode != 0:
        reason = upload_app.stderr.strip() or upload_app.stdout.strip() or "SCP app upload failed"
        return machine_result(machine, "unreachable", reason, remote_tiers, local_dir, "SCP_APP_UPLOAD_FAILED")

    # Clear macOS quarantine flag so Gatekeeper does not block the unsigned app.
    clear_quarantine = run_with_retry(
        ssh_cmd(machine, f"xattr -cr {shlex.quote(remote_app)}"),
        timeout_secs,
        dry_run,
    )
    if clear_quarantine.returncode != 0:
        note = clear_quarantine.stderr.strip() or "xattr -cr failed (non-fatal)"
        write_text(local_dir / "quarantine-clear.log", note)

    # Windows SCP does not preserve Unix execute bits — fix them after upload.
    # Then re-sign ad-hoc because the original signature is invalidated.
    fix_and_sign = run_with_retry(
        ssh_cmd(machine, (
            f"find {shlex.quote(remote_app)} -type f -perm +0111 -name '*.dylib' -exec chmod +x {{}} + 2>/dev/null; "
            f"chmod +x {shlex.quote(remote_app)}/Contents/MacOS/*; "
            f"codesign --force --deep --sign - {shlex.quote(remote_app)} 2>&1"
        )),
        timeout_secs,
        dry_run,
    )
    if fix_and_sign.returncode != 0:
        note = fix_and_sign.stderr.strip() or fix_and_sign.stdout.strip() or "codesign failed (non-fatal)"
        write_text(local_dir / "fix-and-sign.log", note)

    upload_agent = run_with_retry(
        scp_cmd(machine, str(AGENT_SCRIPT), f"{machine.target}:{remote_agent}"),
        timeout_secs,
        dry_run,
    )
    if upload_agent.returncode != 0:
        reason = upload_agent.stderr.strip() or upload_agent.stdout.strip() or "SCP agent upload failed"
        return machine_result(machine, "unreachable", reason, remote_tiers, local_dir, "SCP_AGENT_UPLOAD_FAILED")

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
    if debug:
        agent_args.append("--debug")
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
            "AGENT_TIMEOUT",
        )

    fetch = run_with_retry(
        scp_cmd(machine, f"{machine.target}:{remote_out}/.", str(local_dir)),
        timeout_secs,
        dry_run,
        retries=0,
    )
    if fetch.returncode != 0:
        reason = fetch.stderr.strip() or fetch.stdout.strip() or "SCP result fetch failed"
        return machine_result(machine, "failed", reason, remote_tiers, local_dir, "SCP_RESULT_FETCH_FAILED")

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
        return machine_result(
            machine,
            "failed",
            "remote agent did not produce result.json",
            remote_tiers,
            local_dir,
            "REMOTE_RESULT_MISSING",
        )
    try:
        result = json.loads(result_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return machine_result(
            machine,
            "failed",
            f"remote result.json is invalid JSON: {exc}",
            remote_tiers,
            local_dir,
            "REMOTE_RESULT_INVALID",
        )
    apply_rosetta_gate(result, machine, local_dir, remote_tiers)
    write_text(result_path, json.dumps(result, indent=2) + "\n")
    return result


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
    parser.add_argument("--debug", action="store_true", help="Treat the app as a debug build; skip release-only notarization checks.")
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
    remote_requested = [tier for tier in requested_tiers if tier != "t0"]

    config_path: Path | None = None
    machines: list[Machine] = []
    inventory_error: str | None = None
    if "t0" in requested_tiers or remote_requested:
        config_path, machines, inventory_error = load_machine_inventory_for_run(
            args,
            required=bool(remote_requested),
        )
        if remote_requested and inventory_error:
            print(inventory_error, file=sys.stderr)
            return 2

    print("=== Wavis macOS compatibility run ===")
    print(f"app        : {app_path}")
    print(f"tiers      : {','.join(requested_tiers)}")
    print(f"results    : {results_dir}")
    print(f"parallel   : {args.parallel}")
    print(f"debug      : {args.debug}")

    if "t0" in requested_tiers:
        print("\n--- Tier 0: local package validation ---")
        t0_machines = machines if config_path and not inventory_error else None
        t0_result = run_tier0(app_path, results_dir / "_local", t0_machines, args.debug)
        if config_path and inventory_error:
            note = f"{inventory_error}; skipped binary architecture cross-check"
            t0_result.setdefault("notes", []).append(note)
            write_text(results_dir / "_local" / "result.json", json.dumps({"tiers": {"t0": t0_result}}, indent=2) + "\n")
        state = "PASS" if t0_result["pass"] else "FAIL"
        print(f"T0 {state}")
        for note in t0_result["notes"]:
            print(f"  [warn] {note}")
        for failure in t0_result["failures"]:
            print(f"  [fail] {format_failure(failure)}")

    if remote_requested:
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
                    args.debug,
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
