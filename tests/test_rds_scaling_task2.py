"""Task 2 property tests for the RDS scaling Terraform configuration.

These tests are source-driven instead of plan-driven because generating a real
Terraform plan in local development requires live AWS credentials. The tests
therefore validate the concrete Terraform blocks in ``rds.tf`` and the related
private-subnet block in ``networking.tf``.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[1]
RDS_TF = REPO_ROOT / "infrastructure" / "environments" / "dev" / "rds.tf"
NETWORKING_TF = REPO_ROOT / "infrastructure" / "environments" / "dev" / "networking.tf"

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
def rds_blocks() -> dict[tuple[str, str, str], str]:
    return extract_tf_blocks(RDS_TF)


@pytest.fixture(scope="module")
def networking_blocks() -> dict[tuple[str, str, str], str]:
    return extract_tf_blocks(NETWORKING_TF)


def test_property25_feature_flag_gates_all_rds_resources(
    rds_blocks: dict[tuple[str, str, str], str],
) -> None:
    """Property 1: every task-2 RDS block is gated behind enable_rds."""
    expected_blocks = {
        ("data", "aws_availability_zones", "available"),
        ("data", "aws_subnets", "rds_secondary"),
        ("data", "aws_subnet", "rds_secondary"),
        ("resource", "aws_db_subnet_group", "wavis"),
        ("resource", "aws_security_group", "rds"),
        ("resource", "aws_vpc_security_group_ingress_rule", "rds_postgres"),
        ("resource", "aws_vpc_security_group_egress_rule", "rds_all_out"),
        ("resource", "aws_ssm_parameter", "rds_master_password"),
        ("resource", "aws_ssm_parameter", "enable_rds"),
        ("resource", "aws_db_instance", "wavis"),
    }

    assert set(rds_blocks) == expected_blocks, (
        "rds.tf should contain only the task-2 RDS blocks so feature gating is "
        "easy to reason about"
    )

    for key, block in rds_blocks.items():
        assert "count = var.enable_rds ? 1 : 0" in block, (
            f"{key} is not gated by var.enable_rds"
        )


def test_property26_rds_security_group_allows_only_wavis_sg_ingress(
    rds_blocks: dict[tuple[str, str, str], str],
) -> None:
    """Property 2: the RDS SG exposes only TCP 5432 from the Wavis SG."""
    ingress_blocks = [
        key for key in rds_blocks
        if key[0] == "resource" and key[1] == "aws_vpc_security_group_ingress_rule"
    ]
    assert ingress_blocks == [("resource", "aws_vpc_security_group_ingress_rule", "rds_postgres")], (
        f"Unexpected ingress rule set in rds.tf: {ingress_blocks}"
    )

    ingress = get_block(
        rds_blocks,
        "resource",
        "aws_vpc_security_group_ingress_rule",
        "rds_postgres",
    )
    assert 'security_group_id            = aws_security_group.rds[0].id' in ingress
    assert 'referenced_security_group_id = aws_security_group.wavis.id' in ingress
    assert 'from_port                    = 5432' in ingress
    assert 'to_port                      = 5432' in ingress
    assert 'ip_protocol                  = "tcp"' in ingress

    egress = get_block(
        rds_blocks,
        "resource",
        "aws_vpc_security_group_egress_rule",
        "rds_all_out",
    )
    assert 'cidr_ipv4         = "0.0.0.0/0"' in egress
    assert 'ip_protocol       = "-1"' in egress


def test_property27_all_rds_resources_carry_standard_project_tags(
    rds_blocks: dict[tuple[str, str, str], str],
) -> None:
    """Property 8: every taggable task-2 RDS resource includes local.tags."""
    taggable_blocks = [
        ("resource", "aws_db_subnet_group", "wavis"),
        ("resource", "aws_security_group", "rds"),
        ("resource", "aws_ssm_parameter", "rds_master_password"),
        ("resource", "aws_ssm_parameter", "enable_rds"),
        ("resource", "aws_db_instance", "wavis"),
    ]

    for key in taggable_blocks:
        block = get_block(rds_blocks, *key)
        assert "local.tags" in block, f"{key} is missing the standard local.tags set"


def test_property28_enable_rds_ssm_parameter_conditional_existence(
    rds_blocks: dict[tuple[str, str, str], str],
) -> None:
    """Property 9: ENABLE_RDS is created only when enable_rds=true."""
    enable_rds_param = get_block(
        rds_blocks,
        "resource",
        "aws_ssm_parameter",
        "enable_rds",
    )
    assert "count = var.enable_rds ? 1 : 0" in enable_rds_param
    assert 'name  = "${local.ssm_prefix}/ENABLE_RDS"' in enable_rds_param
    assert 'type  = "String"' in enable_rds_param
    assert 'value = "true"' in enable_rds_param


@pytest.mark.parametrize(
    ("enable_rds", "enable_private_subnet", "expected_primary_subnet_ref"),
    [
        (False, False, None),
        (False, True, None),
        (True, False, "data.aws_subnet.existing.id"),
        (True, True, "aws_subnet.private[0].id"),
    ],
)
def test_property29_rds_subnet_placement_follows_private_subnet_flag(
    rds_blocks: dict[tuple[str, str, str], str],
    networking_blocks: dict[tuple[str, str, str], str],
    enable_rds: bool,
    enable_private_subnet: bool,
    expected_primary_subnet_ref: str | None,
) -> None:
    """Property 10: the DB subnet group switches to the private subnet when enabled."""
    subnet_group = get_block(rds_blocks, "resource", "aws_db_subnet_group", "wavis")
    private_subnet = get_block(networking_blocks, "resource", "aws_subnet", "private")
    secondary_lookup = get_block(rds_blocks, "data", "aws_subnets", "rds_secondary")

    assert "count = var.enable_private_subnet ? 1 : 0" in private_subnet
    assert "try(aws_subnet.private[0].id, data.aws_subnet.existing.id)" in subnet_group
    assert "data.aws_subnet.rds_secondary[0].id" in subnet_group
    assert "if az != data.aws_subnet.existing.availability_zone" in secondary_lookup

    if not enable_rds:
        assert "count = var.enable_rds ? 1 : 0" in subnet_group
        return

    assert expected_primary_subnet_ref is not None
    assert expected_primary_subnet_ref in subnet_group

    if enable_private_subnet:
        assert "aws_subnet.private[0].id" in subnet_group
    else:
        assert "data.aws_subnet.existing.id" in subnet_group
