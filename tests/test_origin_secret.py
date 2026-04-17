"""Integration test: origin secret header rejection.

Property 7: Origin Secret Header Rejection

Uses hypothesis to generate random request paths and HTTP methods, then
sends requests to the backend on port 3000 without the X-Origin-Verify
header and asserts HTTP 403.

**Validates: Requirements 4.5, 9.2**

Usage:
    # Run against a local backend (default localhost:3000):
    pytest tests/test_origin_secret.py -v

    # Override backend URL:
    BACKEND_URL=http://10.0.1.50:3000 pytest tests/test_origin_secret.py -v

Note: These tests require a running backend instance with origin secret
validation enabled (CF_ORIGIN_SECRET configured). They are intended to
be run as part of the post-deploy smoke test suite.
"""

from __future__ import annotations

import os
import urllib.request
import urllib.error

import pytest
from hypothesis import given, settings, assume
from hypothesis import strategies as st

# Mark all tests in this module as integration tests
pytestmark = pytest.mark.integration


BACKEND_URL = os.environ.get("BACKEND_URL", "http://localhost:3000")


# ============================================================================
# Strategies
# ============================================================================

# Generate realistic URL path segments
path_segment = st.from_regex(r"[a-zA-Z0-9_\-]{1,20}", fullmatch=True)

# Generate request paths like /health, /api/rooms, /ws, etc.
request_paths = st.lists(
    path_segment, min_size=1, max_size=4
).map(lambda parts: "/" + "/".join(parts))

# HTTP methods that the backend should reject without the origin header
http_methods = st.sampled_from(["GET", "POST", "PUT", "DELETE", "PATCH"])


# ============================================================================
# Helpers
# ============================================================================

def _send_request(path: str, method: str = "GET", headers: dict | None = None) -> int:
    """Send an HTTP request and return the status code.

    Returns -1 if the connection fails entirely (backend unreachable).
    """
    url = f"{BACKEND_URL}{path}"
    req = urllib.request.Request(url, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return resp.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, OSError):
        return -1


def _backend_reachable() -> bool:
    """Check if the backend is reachable at all."""
    return _send_request("/health") != -1


# ============================================================================
# Property 7 — Origin Secret Header Rejection
# Feature: private-subnet-migration, Property 7: Origin Secret Header Rejection
# Validates: Requirements 4.5, 9.2
# ============================================================================

class TestProperty7OriginSecretRejection:
    """P7: Requests without X-Origin-Verify header are rejected with HTTP 403.

    **Validates: Requirements 4.5, 9.2**
    """

    @pytest.fixture(autouse=True)
    def _check_backend(self) -> None:
        """Skip all tests if backend is not reachable."""
        if not _backend_reachable():
            pytest.skip(
                f"Backend not reachable at {BACKEND_URL} — "
                "run this test with a live backend instance"
            )

    @given(path=request_paths, method=http_methods)
    @settings(max_examples=50)
    def test_requests_without_origin_header_return_403(
        self, path: str, method: str
    ) -> None:
        """Any request without X-Origin-Verify header gets HTTP 403.

        **Validates: Requirements 4.5, 9.2**
        """
        status = _send_request(path, method=method)

        # Backend must reject with 403 when origin secret header is missing
        assert status == 403, (
            f"{method} {path} without X-Origin-Verify returned {status}, expected 403"
        )

    @given(path=request_paths)
    @settings(max_examples=20)
    def test_requests_with_wrong_origin_header_return_403(
        self, path: str
    ) -> None:
        """Requests with an incorrect X-Origin-Verify value get HTTP 403.

        **Validates: Requirements 4.5, 9.2**
        """
        headers = {"X-Origin-Verify": "wrong-secret-value-12345"}
        status = _send_request(path, method="GET", headers=headers)

        assert status == 403, (
            f"GET {path} with wrong X-Origin-Verify returned {status}, expected 403"
        )
