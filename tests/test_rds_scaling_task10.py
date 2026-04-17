"""Task 10 checkpoint tests for the RDS scaling rollout.

Task 10 is an end-to-end validation checkpoint. In local development we can
always verify that Terraform still parses and that the production compose stack
renders correctly with the RDS overlay. The live Terraform plan variants remain
opt-in because they require a real AWS backend and credentials.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import pytest
import yaml


REPO_ROOT = Path(__file__).resolve().parents[1]
TF_DIR = REPO_ROOT / "infrastructure" / "environments" / "dev"


def run_terraform(*args: str) -> subprocess.CompletedProcess[str]:
    """Run Terraform in the dev environment and capture combined output."""
    return subprocess.run(
        ["terraform", *args],
        cwd=TF_DIR,
        text=True,
        capture_output=True,
        check=False,
    )


def format_failure(result: subprocess.CompletedProcess[str]) -> str:
    """Return readable command output for assertion failures."""
    stdout = result.stdout.strip()
    stderr = result.stderr.strip()
    return "\n".join(part for part in [stdout, stderr] if part) or "no output"


def require_plan_checkpoint() -> None:
    """Skip when the environment is not configured for live Terraform plans."""
    if os.environ.get("RUN_TERRAFORM_PLAN") != "1":
        pytest.skip(
            "Task 10 terraform plan checks require live AWS credentials and backend "
            "access; set RUN_TERRAFORM_PLAN=1 to run them"
        )


def compose_env() -> dict[str, str]:
    """Provide the required prod-overlay environment variables for config rendering."""
    env = os.environ.copy()
    env.update(
        {
            "POSTGRES_USER": "wavis",
            "POSTGRES_PASSWORD": "wavis",
            "POSTGRES_DB": "wavis",
            "LIVEKIT_API_KEY": "devkey",
            "LIVEKIT_API_SECRET": "devsecret",
            "RUST_LOG": "info",
            "REQUIRE_TLS": "false",
            "TRUST_PROXY_HEADERS": "false",
            "DATABASE_URL": "postgres://wavis:wavis@db.example:5432/wavis",
            "AUTH_JWT_SECRET": "dev-auth-secret-32-bytes-min!!XX",
            "AUTH_REFRESH_PEPPER": "dev-pepper-32-bytes-minimum!!XXX",
            "PHRASE_ENCRYPTION_KEY": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "PAIRING_CODE_PEPPER": "dev-pairing-pepper-32-bytes!!XXX",
            "SFU_JWT_SECRET": "dev-secret-32-bytes-minimum!!!XX",
            "LIVEKIT_HOST": "ws://livekit:7880",
            "LIVEKIT_PUBLIC_HOST": "ws://localhost:7880",
            "GITHUB_BUG_REPORT_TOKEN": "ghp_dev_placeholder",
            "GITHUB_BUG_REPORT_REPO": "owner/repo",
            "BUG_REPORT_RATE_LIMIT_MAX": "5",
            "BUG_REPORT_RATE_LIMIT_WINDOW_SECS": "3600",
            "BUG_REPORT_LLM_API_KEY": "placeholder",
            "BUG_REPORT_LLM_MODEL": "claude-sonnet-4-20250514",
        }
    )
    return env


def docker_compose_config() -> dict[str, Any]:
    """Render the merged production compose config with the RDS overlay."""
    env = compose_env()
    with tempfile.TemporaryDirectory() as docker_config_dir:
        env["DOCKER_CONFIG"] = docker_config_dir
        result = subprocess.run(
            [
                "docker",
                "compose",
                "-f",
                "docker-compose.yml",
                "-f",
                "docker-compose.prod.yml",
                "-f",
                "docker-compose.rds.yml",
                "config",
            ],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
            env=env,
        )

    assert result.returncode == 0, format_failure(result)
    rendered = yaml.safe_load(result.stdout)
    assert isinstance(rendered, dict), "docker compose config must render a YAML mapping"
    return rendered


def test_task10_terraform_validate_succeeds() -> None:
    """Checkpoint guardrail: the Terraform module must remain syntactically valid."""
    result = run_terraform("validate", "-no-color")
    assert result.returncode == 0, format_failure(result)


def test_task10_rds_overlay_renders_with_postgres_disabled() -> None:
    """Checkpoint: merged compose output must disable Docker Postgres cleanly."""
    rendered = docker_compose_config()
    services = rendered.get("services", {})

    assert isinstance(services, dict), "docker compose config must contain services"
    assert services.get("postgres", {}).get("deploy", {}).get("replicas") == 0

    backend_depends_on = services.get("wavis-backend", {}).get("depends_on", {})
    assert isinstance(backend_depends_on, dict), (
        "wavis-backend depends_on must remain a mapping after compose rendering"
    )
    assert "postgres" not in backend_depends_on, (
        "wavis-backend must not depend on postgres when the RDS overlay is applied"
    )
    assert backend_depends_on.get("livekit", {}).get("condition") == "service_started", (
        "wavis-backend must continue to wait for livekit startup"
    )


@pytest.mark.integration
@pytest.mark.parametrize("enable_rds", ["false", "true"])
def test_task10_terraform_plan_smoke(enable_rds: str) -> None:
    """Credentialed checkpoint: both Terraform plan variants should succeed."""
    require_plan_checkpoint()

    result = run_terraform(
        "plan",
        "-input=false",
        "-lock=false",
        "-no-color",
        f"-var=enable_rds={enable_rds}",
    )
    assert result.returncode == 0, format_failure(result)
