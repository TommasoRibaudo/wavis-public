"""Task 8 property tests for the Docker Compose RDS override.

These tests stay source-driven and validate the raw overlay file, including the
Compose-specific ``!override`` tag used to replace inherited ``depends_on``
entries when RDS mode is enabled.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml


REPO_ROOT = Path(__file__).resolve().parents[1]
BASE_COMPOSE = REPO_ROOT / "docker-compose.yml"
RDS_COMPOSE = REPO_ROOT / "docker-compose.rds.yml"


class ComposeLoader(yaml.SafeLoader):
    """YAML loader that understands Docker Compose merge tags."""


def construct_override(loader: ComposeLoader, node: yaml.Node) -> Any:
    """Treat ``!override`` as the wrapped mapping/sequence/scalar value."""
    if isinstance(node, yaml.MappingNode):
        return loader.construct_mapping(node)
    if isinstance(node, yaml.SequenceNode):
        return loader.construct_sequence(node)
    return loader.construct_scalar(node)


ComposeLoader.add_constructor("!override", construct_override)


def load_yaml(path: Path) -> dict[str, Any]:
    """Load a YAML file into a dictionary."""
    with path.open(encoding="utf-8") as handle:
        data = yaml.load(handle, Loader=ComposeLoader)
    assert isinstance(data, dict), f"{path} did not parse into a YAML mapping"
    return data


def test_task81_rds_override_disables_postgres_and_clears_backend_depends_on() -> None:
    """Task 8.1: the RDS overlay disables Docker Postgres for production deploys."""
    compose = load_yaml(RDS_COMPOSE)
    services = compose.get("services")

    assert isinstance(services, dict), "docker-compose.rds.yml must define services"
    assert set(services) == {"postgres", "wavis-backend"}

    postgres = services["postgres"]
    assert postgres == {"deploy": {"replicas": 0}}, (
        "docker-compose.rds.yml must disable the postgres service with deploy.replicas=0"
    )

    backend = services["wavis-backend"]
    assert backend == {"depends_on": {"livekit": {"condition": "service_started"}}}, (
        "docker-compose.rds.yml must override wavis-backend depends_on so the "
        "postgres health dependency is removed while the livekit startup "
        "dependency remains"
    )


def test_property42_base_compose_remains_the_source_of_local_postgres_dependencies() -> None:
    """Property 4: the base compose file keeps local-development Postgres wiring."""
    compose = load_yaml(BASE_COMPOSE)
    services = compose.get("services")

    assert isinstance(services, dict), "docker-compose.yml must define services"
    assert "postgres" in services, "docker-compose.yml must retain the postgres service"
    assert "wavis-backend" in services, (
        "docker-compose.yml must retain the backend service wired for local development"
    )

    backend_depends_on = services["wavis-backend"].get("depends_on")
    assert backend_depends_on == {
        "postgres": {"condition": "service_healthy"},
        "livekit": {"condition": "service_started"},
    }, (
        "docker-compose.yml should remain unchanged for local development; "
        "wavis-backend must still depend on postgres health and livekit startup"
    )
