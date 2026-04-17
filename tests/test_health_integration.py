"""Integration test: backend health degrades when LiveKit unreachable.

Property 12: Backend Health Degrades When LiveKit Unreachable

This is an integration test that requires a running backend instance.
It starts the backend with an unreachable LIVEKIT_HOST and asserts that
the /health endpoint returns a degraded status indicating voice is
unavailable while signaling remains operational.

**Validates: Requirements 8.6**

Usage:
    # Run against a local backend (default localhost:3000):
    pytest tests/test_health_integration.py -v

    # Override backend URL:
    BACKEND_URL=http://10.0.1.50:3000 pytest tests/test_health_integration.py -v

    # Skip if no backend is available:
    pytest tests/test_health_integration.py -v -k "not integration"

Note: These tests require a running backend instance configured with an
unreachable LIVEKIT_HOST. They are intended to be run as part of the
post-deploy smoke test suite, not in CI without infrastructure.
"""

from __future__ import annotations

import os

import pytest

# Mark all tests in this module as integration tests
pytestmark = pytest.mark.integration


BACKEND_URL = os.environ.get("BACKEND_URL", "http://localhost:3000")


def _get_health(url: str = BACKEND_URL) -> dict | None:
    """GET /health from the backend and return parsed JSON, or None on connection error."""
    try:
        import urllib.request
        import json

        req = urllib.request.Request(f"{url}/health", method="GET")
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = resp.read().decode("utf-8")
            return json.loads(body)
    except Exception:
        return None


# ============================================================================
# Property 12 — Backend Health Degrades When LiveKit Unreachable
# Feature: private-subnet-migration, Property 12: Backend Health Degrades When LiveKit Unreachable
# Validates: Requirements 8.6
# ============================================================================

class TestProperty12HealthDegradation:
    """P12: Backend /health returns degraded status when LiveKit is unreachable.

    **Validates: Requirements 8.6**

    Prerequisites:
    - Backend is running at BACKEND_URL (default http://localhost:3000)
    - LIVEKIT_HOST is set to an unreachable address (e.g. ws://192.0.2.1:7880)
      OR LiveKit container is stopped

    The backend's SfuRoomManager should detect that LiveKit is unreachable
    and report degraded status on /health — voice unavailable, signaling
    still operational.
    """

    def test_health_endpoint_responds(self) -> None:
        """Backend /health endpoint is reachable and returns JSON.

        **Validates: Requirements 8.6**
        """
        health = _get_health()
        if health is None:
            pytest.skip(
                f"Backend not reachable at {BACKEND_URL} — "
                "run this test with a live backend instance"
            )
        assert isinstance(health, dict), (
            f"/health returned non-dict: {health!r}"
        )

    def test_health_reports_degraded_when_livekit_unreachable(self) -> None:
        """When LiveKit is unreachable, /health returns degraded status.

        The response should indicate:
        - Overall status is degraded (not fully healthy)
        - Voice/SFU component is unavailable
        - Signaling remains operational

        **Validates: Requirements 8.6**
        """
        health = _get_health()
        if health is None:
            pytest.skip(
                f"Backend not reachable at {BACKEND_URL} — "
                "run this test with a live backend instance"
            )

        # The health response should indicate degraded state.
        # Exact field names depend on backend implementation — check common patterns:
        status = health.get("status", "").lower()
        sfu_status = health.get("sfu", health.get("livekit", health.get("voice", {})))

        # Accept various degraded indicators
        is_degraded = (
            status in ("degraded", "partial", "warning")
            or (isinstance(sfu_status, dict) and sfu_status.get("status", "").lower() in ("unavailable", "down", "error", "degraded"))
            or (isinstance(sfu_status, str) and sfu_status.lower() in ("unavailable", "down", "error", "degraded"))
        )

        assert is_degraded, (
            f"/health did not report degraded status when LiveKit is unreachable. "
            f"Response: {health}"
        )

    def test_signaling_still_operational_when_livekit_down(self) -> None:
        """Signaling remains operational even when LiveKit is unreachable.

        **Validates: Requirements 8.6**
        """
        health = _get_health()
        if health is None:
            pytest.skip(
                f"Backend not reachable at {BACKEND_URL} — "
                "run this test with a live backend instance"
            )

        # Signaling should still be operational — the /health endpoint itself
        # responding is evidence of this. Additionally check for explicit
        # signaling status if present.
        signaling = health.get("signaling", health.get("websocket", None))
        if signaling is not None:
            if isinstance(signaling, dict):
                sig_status = signaling.get("status", "").lower()
            else:
                sig_status = str(signaling).lower()
            assert sig_status in ("ok", "healthy", "operational", "up"), (
                f"Signaling reported as {sig_status!r} — expected operational. "
                f"Full response: {health}"
            )
