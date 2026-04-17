"""Task 4 checkpoint tests for the RDS scaling Terraform configuration.

Task 4 is operational by nature: the real checkpoint is that ``terraform plan``
succeeds with ``enable_rds`` both disabled and enabled. In local development
that requires live AWS backend/provider credentials, so this module splits the
checkpoint into:

1. an always-on ``terraform validate`` check that catches parser/config errors
2. opt-in integration smoke tests for the two ``terraform plan`` variants

Set ``RUN_TERRAFORM_PLAN=1`` in an environment with working AWS credentials to
exercise the actual plan checkpoint.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest


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


def test_task4_terraform_validate_succeeds() -> None:
    """Checkpoint guardrail: the Terraform module must remain syntactically valid."""
    result = run_terraform("validate", "-no-color")
    assert result.returncode == 0, format_failure(result)


@pytest.mark.integration
@pytest.mark.parametrize("enable_rds", ["false", "true"])
def test_task4_terraform_plan_smoke(enable_rds: str) -> None:
    """Checkpoint smoke test for credentialed environments."""
    if os.environ.get("RUN_TERRAFORM_PLAN") != "1":
        pytest.skip(
            "terraform plan requires live AWS credentials and backend access; "
            "set RUN_TERRAFORM_PLAN=1 to run this checkpoint"
        )

    result = run_terraform(
        "plan",
        "-input=false",
        "-no-color",
        f"-var=enable_rds={enable_rds}",
    )
    assert result.returncode == 0, format_failure(result)
