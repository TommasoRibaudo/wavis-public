"""Unit tests for pure functions in tools/compat/report/merge-report.py."""

from __future__ import annotations

import importlib.util
import json
import sys
import textwrap
from pathlib import Path

import pytest

_MERGE_REPORT = Path(__file__).resolve().parents[1] / "report" / "merge-report.py"
_spec = importlib.util.spec_from_file_location("merge_report", _MERGE_REPORT)
_mod = importlib.util.module_from_spec(_spec)  # type: ignore[arg-type]
sys.modules["merge_report"] = _mod
_spec.loader.exec_module(_mod)  # type: ignore[union-attr]

parse_issue = _mod.parse_issue
proc_translated_is_active = _mod.proc_translated_is_active
tier_passes = _mod.tier_passes
first_failure_note = _mod.first_failure_note
first_failing_phase_and_category = _mod.first_failing_phase_and_category
build_report = _mod.build_report
markdown_summary = _mod.markdown_summary
FAILURE_CATEGORIES = _mod.FAILURE_CATEGORIES


# ---------------------------------------------------------------------------
# parse_issue
# ---------------------------------------------------------------------------

class TestParseIssue:
    def test_string_with_code_and_message(self):
        assert parse_issue("IPC_FAILED: bridge timed out") == ("IPC_FAILED", "bridge timed out")

    def test_string_without_code_prefix(self):
        code, msg = parse_issue("something went wrong")
        assert code == ""
        assert msg == "something went wrong"

    def test_string_code_only(self):
        # A string that looks like a code but has nothing after the colon.
        code, msg = parse_issue("SCK_VERSION_WRONG: ")
        assert code == "SCK_VERSION_WRONG"
        assert msg == ""

    def test_dict_form(self):
        assert parse_issue({"code": "LAUNCH_CRASH", "message": "crash report found"}) == (
            "LAUNCH_CRASH",
            "crash report found",
        )

    def test_dict_missing_code(self):
        code, msg = parse_issue({"message": "only a message"})
        assert code == ""
        assert msg == "only a message"

    def test_dict_missing_message(self):
        code, msg = parse_issue({"code": "ARCH_MISMATCH"})
        assert code == "ARCH_MISMATCH"
        assert msg == ""

    def test_code_must_start_uppercase(self):
        # Lowercase prefix should not be treated as a code.
        code, msg = parse_issue("note: something")
        assert code == ""

    def test_multiword_message_preserved(self):
        code, msg = parse_issue("DEPLOYMENT_TARGET_MISMATCH: binary is 13.0, expected 10.15")
        assert code == "DEPLOYMENT_TARGET_MISMATCH"
        assert msg == "binary is 13.0, expected 10.15"


# ---------------------------------------------------------------------------
# proc_translated_is_active
# ---------------------------------------------------------------------------

class TestProcTranslatedIsActive:
    def test_int_one(self):
        assert proc_translated_is_active(1) is True

    def test_int_zero(self):
        assert proc_translated_is_active(0) is False

    def test_bool_true(self):
        assert proc_translated_is_active(True) is True

    def test_bool_false(self):
        assert proc_translated_is_active(False) is False

    def test_string_one(self):
        assert proc_translated_is_active("1") is True

    def test_string_zero(self):
        assert proc_translated_is_active("0") is False

    def test_string_one_with_whitespace(self):
        assert proc_translated_is_active(" 1 ") is True

    def test_other_string(self):
        assert proc_translated_is_active("yes") is False


# ---------------------------------------------------------------------------
# tier_passes
# ---------------------------------------------------------------------------

class TestTierPasses:
    def test_all_pass(self):
        result = {"tiers": {"t0": {"pass": True}, "t1": {"pass": True}}}
        assert tier_passes(result) is True

    def test_one_fails(self):
        result = {"tiers": {"t0": {"pass": True}, "t1": {"pass": False}}}
        assert tier_passes(result) is False

    def test_empty_tiers(self):
        assert tier_passes({"tiers": {}}) is False

    def test_missing_tiers_key(self):
        assert tier_passes({}) is False

    def test_pass_false_explicit(self):
        result = {"tiers": {"t0": {"pass": False}}}
        assert tier_passes(result) is False

    def test_pass_missing_treated_as_falsy(self):
        result = {"tiers": {"t0": {}}}
        assert tier_passes(result) is False


# ---------------------------------------------------------------------------
# first_failure_note
# ---------------------------------------------------------------------------

class TestFirstFailureNote:
    def test_failures_take_priority_over_notes(self):
        tier = {
            "failures": ["IPC_FAILED: timeout"],
            "notes": ["some note"],
        }
        note = first_failure_note(tier)
        assert "IPC_FAILED" in note

    def test_falls_back_to_notes_when_no_failures(self):
        tier = {"failures": [], "notes": ["audio_devices was empty"]}
        assert first_failure_note(tier) == "audio_devices was empty"

    def test_empty_tier(self):
        assert first_failure_note({}) == ""

    def test_dict_failure_formatted(self):
        tier = {"failures": [{"code": "LAUNCH_CRASH", "message": "1 crash report"}]}
        note = first_failure_note(tier)
        assert "LAUNCH_CRASH" in note
        assert "1 crash report" in note


# ---------------------------------------------------------------------------
# first_failing_phase_and_category
# ---------------------------------------------------------------------------

class TestFirstFailingPhaseAndCategory:
    def _make_result(self, tier_outcomes: dict[str, bool | list]) -> dict:
        """Build a minimal result dict. Values are True/False or a list of failure strings."""
        tiers = {}
        for tier, value in tier_outcomes.items():
            if isinstance(value, bool):
                tiers[tier] = {"pass": value, "failures": []}
            else:
                tiers[tier] = {"pass": False, "failures": value}
        return {"tiers": tiers}

    def test_all_pass_returns_none(self):
        result = self._make_result({"t0": True, "t1": True})
        phase, category = first_failing_phase_and_category(result)
        assert phase is None
        assert category is None

    def test_t0_fails_first_even_when_t1_also_fails(self):
        result = self._make_result({"t0": False, "t1": False})
        phase, _ = first_failing_phase_and_category(result)
        assert phase == "t0"

    def test_t1_fails_t0_passes(self):
        result = self._make_result({"t0": True, "t1": False})
        phase, _ = first_failing_phase_and_category(result)
        assert phase == "t1"

    def test_category_from_known_failure_code(self):
        result = self._make_result({
            "t0": ["DEPLOYMENT_TARGET_MISMATCH: binary is 13.0, expected 10.15"]
        })
        _, category = first_failing_phase_and_category(result)
        assert category == FAILURE_CATEGORIES["DEPLOYMENT_TARGET_MISMATCH"]

    def test_sck_failure_category(self):
        result = self._make_result({
            "t1": ["SCK_VERSION_WRONG: status was strong on 11.5.2"]
        })
        _, category = first_failing_phase_and_category(result)
        assert category == "screencapturekit_compatibility"

    def test_ipc_failure_category(self):
        result = self._make_result({"t2": ["IPC_FAILED: ipc_ok was false"]})
        _, category = first_failing_phase_and_category(result)
        assert category == "ipc_bridge"

    def test_unknown_code_gives_unknown_category(self):
        result = self._make_result({"t1": ["TOTALLY_NEW_CODE: something new"]})
        _, category = first_failing_phase_and_category(result)
        assert category == "unknown"

    def test_unstructured_failure_string(self):
        result = self._make_result({"t1": ["no prefix code here"]})
        _, category = first_failing_phase_and_category(result)
        assert category == "unstructured_failure"

    def test_tier_ordering_t2_before_t3(self):
        result = self._make_result({"t2": False, "t3": False})
        phase, _ = first_failing_phase_and_category(result)
        assert phase == "t2"


# ---------------------------------------------------------------------------
# FAILURE_CATEGORIES completeness
# ---------------------------------------------------------------------------

class TestFailureCategories:
    def test_all_expected_codes_present(self):
        """The codes we document in the spec must all be categorised."""
        must_have = {
            "SCK_VERSION_WRONG",
            "SCK_HARD_LINKED",
            "TAP_VERSION_WRONG",
            "IPC_FAILED",
            "IPC_TIMEOUT",
            "LAUNCH_CRASH",
            "LAUNCH_NOT_RUNNING",
            "DEPLOYMENT_TARGET_MISMATCH",
            "ARCH_MISMATCH",
            "CODESIGN_INVALID",
        }
        missing = must_have - FAILURE_CATEGORIES.keys()
        assert not missing, f"failure codes not in FAILURE_CATEGORIES: {sorted(missing)}"


# ---------------------------------------------------------------------------
# build_report / markdown_summary — fixture-driven integration
# ---------------------------------------------------------------------------

def _write_result(directory: Path, result: dict) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "result.json").write_text(json.dumps(result), encoding="utf-8")


class TestBuildReportAndMarkdown:
    def _make_machine_result(
        self,
        name: str,
        t1_pass: bool,
        t1_failures: list | None = None,
    ) -> dict:
        return {
            "machine": {"name": name},
            "status": "ok",
            "tiers": {
                "t1": {
                    "pass": t1_pass,
                    "failures": t1_failures or [],
                    "notes": [],
                }
            },
        }

    def test_all_pass_summary(self, tmp_path):
        results_dir = tmp_path / "compat-results"
        _write_result(
            results_dir / "mac-ventura",
            self._make_machine_result("mac-ventura", t1_pass=True),
        )
        report = build_report(results_dir, "Wavis.app", "abc123")
        assert report["summary"]["total"] == 1
        assert report["summary"]["passed_all_tiers"] == 1
        assert report["summary"]["failed"] == 0
        assert report["summary"]["failure_machines"] == []

    def test_one_failure_surfaced(self, tmp_path):
        results_dir = tmp_path / "compat-results"
        _write_result(
            results_dir / "mac-bigsur",
            self._make_machine_result(
                "mac-bigsur",
                t1_pass=False,
                t1_failures=["LAUNCH_CRASH: 1 crash report captured"],
            ),
        )
        report = build_report(results_dir, "Wavis.app", "abc123")
        assert report["summary"]["failed"] == 1
        assert "mac-bigsur" in report["summary"]["failure_machines"]

    def test_markdown_contains_machine_name(self, tmp_path):
        results_dir = tmp_path / "compat-results"
        _write_result(
            results_dir / "mac-ventura",
            self._make_machine_result("mac-ventura", t1_pass=True),
        )
        report = build_report(results_dir, "Wavis.app", "abc123")
        md = markdown_summary(report)
        assert "mac-ventura" in md

    def test_markdown_contains_failure_note_without_raw_logs(self, tmp_path):
        """Acceptance criterion: failure note visible in Markdown, no raw log path needed."""
        results_dir = tmp_path / "compat-results"
        _write_result(
            results_dir / "mac-bigsur",
            self._make_machine_result(
                "mac-bigsur",
                t1_pass=False,
                t1_failures=["SCK_VERSION_WRONG: status was strong on 11.5.2"],
            ),
        )
        report = build_report(results_dir, "Wavis.app", "abc123")
        md = markdown_summary(report)
        assert "SCK_VERSION_WRONG" in md

    def test_markdown_pass_and_fail_labels(self, tmp_path):
        results_dir = tmp_path / "compat-results"
        _write_result(
            results_dir / "mac-ventura",
            self._make_machine_result("mac-ventura", t1_pass=True),
        )
        _write_result(
            results_dir / "mac-bigsur",
            self._make_machine_result("mac-bigsur", t1_pass=False, t1_failures=["LAUNCH_CRASH: crash"]),
        )
        report = build_report(results_dir, "Wavis.app", "abc123")
        md = markdown_summary(report)
        assert "PASS" in md
        assert "FAIL" in md

    def test_missing_result_json_still_produces_report(self, tmp_path):
        results_dir = tmp_path / "compat-results"
        (results_dir / "mac-broken").mkdir(parents=True)  # dir exists but no result.json
        report = build_report(results_dir, "Wavis.app", "abc123")
        assert report["summary"]["total"] == 1
        assert report["summary"]["failed"] == 1

    def test_app_sha_in_report(self, tmp_path):
        results_dir = tmp_path / "compat-results"
        _write_result(
            results_dir / "mac-ventura",
            self._make_machine_result("mac-ventura", t1_pass=True),
        )
        report = build_report(results_dir, "Wavis.app", "deadbeef")
        assert report["app_sha"] == "deadbeef"
