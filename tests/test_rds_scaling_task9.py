"""Task 9 tests for the deploy workflow's conditional RDS overlay.

These tests stay source-driven and validate the inline deploy script in the
GitHub Actions workflow without requiring a runner, Docker, or AWS access.
"""

from __future__ import annotations

import re
from pathlib import Path

import yaml
import pytest


REPO_ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_PATH = REPO_ROOT / ".github" / "workflows" / "deploy-dev-ec2.yml"


def load_workflow() -> dict:
    """Parse and return the deploy workflow YAML."""
    with WORKFLOW_PATH.open(encoding="utf-8") as handle:
        workflow = yaml.safe_load(handle)
    assert isinstance(workflow, dict), "deploy-dev-ec2.yml must parse into a mapping"
    return workflow


def get_deploy_run_block() -> str:
    """Return the inline shell script for the deploy step."""
    workflow = load_workflow()
    deploy_job = workflow.get("jobs", {}).get("deploy", {})
    steps = deploy_job.get("steps", [])

    for step in steps:
        if isinstance(step, dict) and step.get("name") == "Deploy":
            run_block = step.get("run", "")
            assert isinstance(run_block, str) and run_block.strip(), (
                "Deploy step must contain a non-empty run block"
            )
            return run_block

    raise AssertionError("No Deploy step found in deploy-dev-ec2.yml")


def compose_command_for_env(env_contents: str) -> str:
    """Mirror the workflow's grep gate for ENABLE_RDS."""
    compose_cmd = "docker compose -f docker-compose.yml -f docker-compose.prod.yml"
    if re.search(r"^ENABLE_RDS=true", env_contents, flags=re.MULTILINE):
        compose_cmd += " -f docker-compose.rds.yml"
    return compose_cmd


def test_task91_workflow_uses_conditional_rds_overlay() -> None:
    """Task 9.1: the deploy step conditionally adds the RDS compose overlay."""
    run_block = get_deploy_run_block()

    assert "bash deploy/fetch-ssm-env.sh .env" in run_block
    assert 'COMPOSE_CMD="docker compose -f docker-compose.yml -f docker-compose.prod.yml"' in run_block
    assert "if grep -q '^ENABLE_RDS=true' .env 2>/dev/null; then" in run_block
    assert 'COMPOSE_CMD="$COMPOSE_CMD -f docker-compose.rds.yml"' in run_block
    assert "$COMPOSE_CMD up -d --build --wait" in run_block
    assert "docker compose ps" in run_block
    assert "curl --fail http://localhost:3000/health" in run_block


@pytest.mark.parametrize(
    ("env_contents", "expects_rds_overlay"),
    [
        ("ENABLE_RDS=true\n", True),
        ("ENABLE_RDS=false\n", False),
        ("ENABLE_RDS=\n", False),
        ("ENABLE_RDS= true\n", False),
        ("ENABLE_RDS=true \n", True),
        ("ENABLE_RDS=True\n", False),
        ("ENABLE_RDS=yes\n", False),
        ("ENABLE_RDS=true-but-not-really\n", True),
        ("DATABASE_URL=postgres://example\n", False),
        ("DATABASE_URL=postgres://example\nENABLE_RDS=true\n", True),
        ("ENABLE_RDS=true\nDATABASE_URL=postgres://example\n", True),
        ("# ENABLE_RDS=true\n", False),
        ("ENABLE_RDS=true\nENABLE_RDS=false\n", True),
        ("", False),
    ],
)
def test_property5_enable_rds_gate_controls_whether_the_overlay_is_added(
    env_contents: str,
    expects_rds_overlay: bool,
) -> None:
    """Property 5: a line starting with ENABLE_RDS=true enables the RDS overlay."""
    command = compose_command_for_env(env_contents)

    assert command.startswith(
        "docker compose -f docker-compose.yml -f docker-compose.prod.yml"
    )
    assert ("-f docker-compose.rds.yml" in command) is expects_rds_overlay
