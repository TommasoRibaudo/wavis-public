"""Task 7 checkpoint tests for full Terraform validation.

Task 7 is the first checkpoint that needs more than a parser-level validation:
we need the real Terraform plan to prove that enabling RDS produces the full
resource/output surface, and disabling it removes the RDS resources entirely.

Because this Terraform module uses a live AWS S3 backend plus AWS data sources,
the plan assertions are opt-in and only run when ``RUN_TERRAFORM_PLAN=1`` is
set in a credentialed environment.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import pytest


REPO_ROOT = Path(__file__).resolve().parents[1]
TF_DIR = REPO_ROOT / "infrastructure" / "environments" / "dev"

EXPECTED_RDS_RESOURCE_PREFIXES = {
    "aws_db_subnet_group.wavis[0]",
    "aws_security_group.rds[0]",
    "aws_vpc_security_group_ingress_rule.rds_postgres[0]",
    "aws_vpc_security_group_egress_rule.rds_all_out[0]",
    "aws_ssm_parameter.rds_master_password[0]",
    "aws_ssm_parameter.enable_rds[0]",
    "aws_db_instance.wavis[0]",
    "aws_cloudwatch_metric_alarm.rds_cpu[0]",
    "aws_cloudwatch_metric_alarm.rds_memory[0]",
    "aws_cloudwatch_metric_alarm.rds_connections[0]",
}

EXPECTED_SHARED_MONITORING_PREFIXES = {
    "aws_sns_topic.alerts[0]",
    "aws_cloudwatch_metric_alarm.ec2_cpu_credit[0]",
}

EXPECTED_RDS_OUTPUTS = {
    "sns_alerts_topic_arn",
    "ec2_cpu_credit_alarm_arn",
    "rds_endpoint",
    "rds_port",
    "rds_db_name",
    "rds_security_group_id",
    "rds_cpu_alarm_arn",
    "rds_memory_alarm_arn",
    "rds_connections_alarm_arn",
}


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
            "Task 7 requires a live terraform plan against AWS; "
            "set RUN_TERRAFORM_PLAN=1 in a credentialed environment"
        )


def terraform_plan_json(enable_rds: bool) -> dict[str, Any]:
    """Generate and return ``terraform show -json`` for a plan variant."""
    require_plan_checkpoint()

    with tempfile.TemporaryDirectory() as temp_dir:
        plan_path = Path(temp_dir) / "task7.plan"

        plan_result = run_terraform(
            "plan",
            "-input=false",
            "-lock=false",
            "-no-color",
            f"-var=enable_rds={'true' if enable_rds else 'false'}",
            f"-out={plan_path}",
        )
        assert plan_result.returncode == 0, format_failure(plan_result)

        show_result = run_terraform("show", "-json", str(plan_path))
        assert show_result.returncode == 0, format_failure(show_result)
        return json.loads(show_result.stdout)


def flatten_planned_resources(module: dict[str, Any] | None) -> list[dict[str, Any]]:
    """Recursively collect planned resources from root and child modules."""
    if not module:
        return []

    resources = list(module.get("resources", []))
    for child in module.get("child_modules", []):
        resources.extend(flatten_planned_resources(child))
    return resources


def planned_resource_addresses(plan: dict[str, Any]) -> set[str]:
    """Collect all managed resource addresses present in planned values."""
    root_module = plan.get("planned_values", {}).get("root_module")
    return {
        resource.get("address", "")
        for resource in flatten_planned_resources(root_module)
        if resource.get("mode") == "managed"
    }


def planned_output_names(plan: dict[str, Any]) -> set[str]:
    """Collect all output names present in planned values."""
    outputs = plan.get("planned_values", {}).get("outputs", {})
    return set(outputs)


@pytest.mark.integration
def test_task7_plan_with_rds_enabled_exposes_expected_resources_and_outputs() -> None:
    """Task 7: enable_rds=true must surface the full RDS topology in the plan."""
    plan = terraform_plan_json(enable_rds=True)
    addresses = planned_resource_addresses(plan)
    outputs = planned_output_names(plan)

    missing_resources = EXPECTED_RDS_RESOURCE_PREFIXES - addresses
    assert not missing_resources, (
        "enable_rds=true plan is missing expected RDS resources: "
        f"{sorted(missing_resources)}"
    )

    missing_shared_monitoring = EXPECTED_SHARED_MONITORING_PREFIXES - addresses
    assert not missing_shared_monitoring, (
        "enable_rds=true plan is missing shared monitoring resources: "
        f"{sorted(missing_shared_monitoring)}"
    )

    missing_outputs = EXPECTED_RDS_OUTPUTS - outputs
    assert not missing_outputs, (
        "enable_rds=true plan is missing expected outputs: "
        f"{sorted(missing_outputs)}"
    )


@pytest.mark.integration
def test_task7_plan_with_rds_disabled_has_zero_rds_resources() -> None:
    """Task 7: enable_rds=false must not plan any RDS resources."""
    plan = terraform_plan_json(enable_rds=False)
    addresses = planned_resource_addresses(plan)

    unexpected_rds_resources = sorted(
        address
        for address in addresses
        if any(address.startswith(prefix) for prefix in EXPECTED_RDS_RESOURCE_PREFIXES)
    )
    assert not unexpected_rds_resources, (
        "enable_rds=false plan should not contain RDS resources, but found: "
        f"{unexpected_rds_resources}"
    )
