"""Shared fixtures for Terraform plan JSON validation tests.

Loads a pre-generated plan JSON file (from ``terraform show -json plan.out``)
and provides helpers to extract resources, SG rules, and IAM policy documents.

Configure the plan file path via the ``TF_PLAN_JSON`` environment variable or
the ``--plan-json`` pytest CLI option.  Defaults to ``plan.json`` in the
current working directory.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import pytest


# ---------------------------------------------------------------------------
# pytest CLI option
# ---------------------------------------------------------------------------

def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--plan-json",
        action="store",
        default=None,
        help="Path to terraform show -json plan.out output file",
    )


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers",
        "integration: marks tests that require live infrastructure (deselect with '-m \"not integration\"')",
    )


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def plan_json_path(request: pytest.FixtureRequest) -> Path:
    """Resolve the plan JSON file path from CLI option or env var."""
    cli = request.config.getoption("--plan-json")
    path_str = cli or os.environ.get("TF_PLAN_JSON", "plan.json")
    return Path(path_str)


@pytest.fixture(scope="session")
def plan(plan_json_path: Path) -> dict[str, Any]:
    """Load and return the full Terraform plan JSON."""
    if not plan_json_path.exists():
        pytest.skip(f"Plan JSON not found at {plan_json_path}")
    with open(plan_json_path, encoding="utf-8") as f:
        return json.load(f)
