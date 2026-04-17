"""Task 3 property tests for the RDS scaling SSM configuration.

These tests are source-driven because the Terraform module depends on live AWS
credentials for a real plan. The assertions here lock down the Task 3
DATABASE_URL wiring and the defensive try(...) guards required by the design.
"""

from __future__ import annotations

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
SSM_TF = REPO_ROOT / "infrastructure" / "environments" / "dev" / "ssm.tf"


def test_property32_database_url_reflects_rds_endpoint_when_enabled() -> None:
    """Property 3: DATABASE_URL switches between Docker Postgres and RDS."""
    ssm_tf = SSM_TF.read_text(encoding="utf-8")

    assert "DATABASE_URL = var.enable_rds" in ssm_tf
    assert '? "postgres://${var.rds_master_username}:${try(aws_ssm_parameter.rds_master_password[0].value, "")}@${try(aws_db_instance.wavis[0].endpoint, "")}/${var.rds_db_name}"' in ssm_tf
    assert ': "postgres://wavis:wavis@localhost:5432/wavis"' in ssm_tf


def test_task3_notes_ignore_changes_behavior_is_still_present() -> None:
    """Task 3 depends on the existing SSM lifecycle behavior staying intact."""
    ssm_tf = SSM_TF.read_text(encoding="utf-8")

    assert 'resource "aws_ssm_parameter" "secrets"' in ssm_tf
    assert "ignore_changes = [value]" in ssm_tf
