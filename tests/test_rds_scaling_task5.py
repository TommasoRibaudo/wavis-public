"""Task 5 property tests for the RDS scaling monitoring configuration.

These tests stay source-driven because a real Terraform plan depends on live
AWS credentials and backend access. They lock down the monitoring wiring added
in Task 5: the widened SNS condition, the new EC2 CPU credit alarm, and the
RDS CloudWatch alarms that all publish through the shared alerts topic.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[1]
MONITORING_TF = REPO_ROOT / "infrastructure" / "environments" / "dev" / "monitoring.tf"

BLOCK_START_RE = re.compile(r'^(resource|data)\s+"([^"]+)"\s+"([^"]+)"\s*\{$')


def extract_tf_blocks(path: Path) -> dict[tuple[str, str, str], str]:
    """Return Terraform resource/data blocks keyed by (kind, type, name)."""
    lines = path.read_text(encoding="utf-8").splitlines()
    blocks: dict[tuple[str, str, str], str] = {}
    index = 0

    while index < len(lines):
        match = BLOCK_START_RE.match(lines[index].strip())
        if not match:
            index += 1
            continue

        kind, block_type, name = match.groups()
        start = index
        depth = lines[index].count("{") - lines[index].count("}")
        index += 1

        while index < len(lines) and depth > 0:
            depth += lines[index].count("{") - lines[index].count("}")
            index += 1

        blocks[(kind, block_type, name)] = "\n".join(lines[start:index])

    return blocks


def get_block(
    blocks: dict[tuple[str, str, str], str],
    kind: str,
    block_type: str,
    name: str,
) -> str:
    key = (kind, block_type, name)
    assert key in blocks, f"Missing Terraform block: {key}"
    return blocks[key]


@pytest.fixture(scope="module")
def monitoring_blocks() -> dict[tuple[str, str, str], str]:
    return extract_tf_blocks(MONITORING_TF)


def test_task52_and_53_alarm_definitions_match_the_spec(
    monitoring_blocks: dict[tuple[str, str, str], str],
) -> None:
    """Task 5 implementation: alarm metrics, thresholds, and gating stay intact."""
    alerts_topic = get_block(monitoring_blocks, "resource", "aws_sns_topic", "alerts")
    assert 'count = (var.enable_private_subnet || var.enable_rds) ? 1 : 0' in alerts_topic

    email_subscription = get_block(
        monitoring_blocks,
        "resource",
        "aws_sns_topic_subscription",
        "email",
    )
    assert 'count = (var.enable_private_subnet || var.enable_rds) && var.alert_email != "" ? 1 : 0' in email_subscription

    ec2_credit = get_block(
        monitoring_blocks,
        "resource",
        "aws_cloudwatch_metric_alarm",
        "ec2_cpu_credit",
    )
    assert 'count = (var.enable_private_subnet || var.enable_rds) ? 1 : 0' in ec2_credit
    assert 'namespace           = "AWS/EC2"' in ec2_credit
    assert 'metric_name         = "CPUCreditBalance"' in ec2_credit
    assert 'comparison_operator = "LessThanThreshold"' in ec2_credit
    assert "threshold           = var.cpu_credit_alarm_threshold" in ec2_credit
    assert "period              = 300" in ec2_credit
    assert "evaluation_periods  = 2" in ec2_credit
    assert "statistic           = \"Minimum\"" in ec2_credit
    assert "InstanceId = aws_instance.wavis.id" in ec2_credit
    assert "local.tags" in ec2_credit

    rds_cpu = get_block(
        monitoring_blocks,
        "resource",
        "aws_cloudwatch_metric_alarm",
        "rds_cpu",
    )
    assert "count = var.enable_rds ? 1 : 0" in rds_cpu
    assert 'namespace           = "AWS/RDS"' in rds_cpu
    assert 'metric_name         = "CPUUtilization"' in rds_cpu
    assert 'comparison_operator = "GreaterThanThreshold"' in rds_cpu
    assert "threshold           = 80" in rds_cpu
    assert "period              = 300" in rds_cpu
    assert "evaluation_periods  = 3" in rds_cpu
    assert "DBInstanceIdentifier = aws_db_instance.wavis[0].id" in rds_cpu
    assert "local.tags" in rds_cpu

    rds_memory = get_block(
        monitoring_blocks,
        "resource",
        "aws_cloudwatch_metric_alarm",
        "rds_memory",
    )
    assert "count = var.enable_rds ? 1 : 0" in rds_memory
    assert 'namespace           = "AWS/RDS"' in rds_memory
    assert 'metric_name         = "FreeableMemory"' in rds_memory
    assert 'comparison_operator = "LessThanThreshold"' in rds_memory
    assert "threshold           = 134217728" in rds_memory
    assert "period              = 300" in rds_memory
    assert "evaluation_periods  = 2" in rds_memory
    assert "DBInstanceIdentifier = aws_db_instance.wavis[0].id" in rds_memory
    assert "local.tags" in rds_memory

    rds_connections = get_block(
        monitoring_blocks,
        "resource",
        "aws_cloudwatch_metric_alarm",
        "rds_connections",
    )
    assert "count = var.enable_rds ? 1 : 0" in rds_connections
    assert 'namespace           = "AWS/RDS"' in rds_connections
    assert 'metric_name         = "DatabaseConnections"' in rds_connections
    assert 'comparison_operator = "GreaterThanThreshold"' in rds_connections
    assert "threshold           = var.rds_max_connections_threshold" in rds_connections
    assert "period              = 300" in rds_connections
    assert "evaluation_periods  = 2" in rds_connections
    assert "DBInstanceIdentifier = aws_db_instance.wavis[0].id" in rds_connections
    assert "local.tags" in rds_connections


@pytest.mark.parametrize(
    ("enable_private_subnet", "enable_rds", "expected_topic"),
    [
        (False, False, False),
        (True, False, True),
        (False, True, True),
        (True, True, True),
    ],
)
def test_property54_sns_topic_exists_when_either_feature_flag_is_true(
    monitoring_blocks: dict[tuple[str, str, str], str],
    enable_private_subnet: bool,
    enable_rds: bool,
    expected_topic: bool,
) -> None:
    """Property 6: the shared SNS topic exists iff either feature flag is enabled."""
    alerts_topic = get_block(monitoring_blocks, "resource", "aws_sns_topic", "alerts")
    email_subscription = get_block(
        monitoring_blocks,
        "resource",
        "aws_sns_topic_subscription",
        "email",
    )

    assert 'count = (var.enable_private_subnet || var.enable_rds) ? 1 : 0' in alerts_topic
    assert 'count = (var.enable_private_subnet || var.enable_rds) && var.alert_email != "" ? 1 : 0' in email_subscription

    assert ((enable_private_subnet or enable_rds) is expected_topic)


def test_property55_all_cloudwatch_alarms_reference_the_shared_sns_topic(
    monitoring_blocks: dict[tuple[str, str, str], str],
) -> None:
    """Property 7: every alarm publishes alarm and recovery events to SNS."""
    expected_alarm_names = {
        "nat_error_port_alloc",
        "nat_packets_drop",
        "ec2_cpu_credit",
        "rds_cpu",
        "rds_memory",
        "rds_connections",
    }

    alarm_blocks = {
        name: block
        for (kind, block_type, name), block in monitoring_blocks.items()
        if kind == "resource" and block_type == "aws_cloudwatch_metric_alarm"
    }

    assert set(alarm_blocks) == expected_alarm_names

    for name, block in alarm_blocks.items():
        assert "alarm_actions = [aws_sns_topic.alerts[0].arn]" in block, (
            f"{name} is missing SNS alarm_actions"
        )
        assert "ok_actions    = [aws_sns_topic.alerts[0].arn]" in block, (
            f"{name} is missing SNS ok_actions"
        )
