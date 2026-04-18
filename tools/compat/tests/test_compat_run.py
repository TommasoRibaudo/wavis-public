"""Unit tests for pure functions in tools/compat/compat-run.py."""

from __future__ import annotations

import argparse
import sys
import textwrap
from pathlib import Path

import pytest

# Allow importing compat-run despite the hyphen in the filename.
import importlib.util

_COMPAT_RUN = Path(__file__).resolve().parents[1] / "compat-run.py"
_spec = importlib.util.spec_from_file_location("compat_run", _COMPAT_RUN)
_mod = importlib.util.module_from_spec(_spec)  # type: ignore[arg-type]
sys.modules["compat_run"] = _mod
_spec.loader.exec_module(_mod)  # type: ignore[union-attr]

parse_tiers = _mod.parse_tiers
compare_versions = _mod.compare_versions
parse_deployment_versions = _mod.parse_deployment_versions
parse_sck_link_type = _mod.parse_sck_link_type
parse_lipo_arches = _mod.parse_lipo_arches
normalize_arch = _mod.normalize_arch
load_machines = _mod.load_machines
Machine = _mod.Machine


# ---------------------------------------------------------------------------
# parse_tiers
# ---------------------------------------------------------------------------

class TestParseTiers:
    def test_single_tier(self):
        assert parse_tiers("t1") == ["t1"]

    def test_two_tiers(self):
        assert parse_tiers("t0,t1") == ["t0", "t1"]

    def test_all_four_tiers(self):
        assert parse_tiers("t0,t1,t2,t3") == ["t0", "t1", "t2", "t3"]

    def test_whitespace_around_commas(self):
        assert parse_tiers("t1, t2") == ["t1", "t2"]

    def test_empty_string_returns_default(self):
        assert parse_tiers("") == ["t0", "t1"]

    def test_unknown_tier_raises(self):
        with pytest.raises(argparse.ArgumentTypeError, match="t4"):
            parse_tiers("t4")

    def test_mix_valid_and_invalid_raises(self):
        with pytest.raises(argparse.ArgumentTypeError):
            parse_tiers("t1,t99")

    def test_order_preserved(self):
        # The function should preserve the order given, not sort.
        assert parse_tiers("t3,t1") == ["t3", "t1"]


# ---------------------------------------------------------------------------
# compare_versions
# ---------------------------------------------------------------------------

class TestCompareVersions:
    def test_equal(self):
        assert compare_versions("10.15", "10.15") == 0

    def test_equal_three_parts(self):
        assert compare_versions("11.5.2", "11.5.2") == 0

    def test_left_less(self):
        assert compare_versions("10.15", "12.3") == -1

    def test_left_greater(self):
        assert compare_versions("13.0", "10.15") == 1

    def test_major_only_less(self):
        assert compare_versions("11", "12") == -1

    def test_major_only_greater(self):
        assert compare_versions("14", "13") == 1

    def test_short_vs_long_equal(self):
        # "12" and "12.0" are the same version.
        assert compare_versions("12", "12.0") == 0

    def test_minor_difference(self):
        assert compare_versions("12.2", "12.3") == -1
        assert compare_versions("12.4", "12.3") == 1

    def test_patch_difference(self):
        assert compare_versions("11.5.1", "11.5.2") == -1

    def test_deployment_target_boundary(self):
        # 11.5.2 is before the 12.3 SCK boundary.
        assert compare_versions("11.5.2", "12.3") == -1

    def test_ventura_passes_sck_boundary(self):
        assert compare_versions("13.6", "12.3") == 1


# ---------------------------------------------------------------------------
# parse_deployment_versions
# ---------------------------------------------------------------------------

def _lc_build_version_block(minos: str) -> str:
    return textwrap.dedent(f"""\
        Load command 42
                  cmd LC_BUILD_VERSION
              cmdsize 32
             platform macos
                minos {minos}
                  sdk 14.2
    """)


def _lc_version_min_block(version: str) -> str:
    return textwrap.dedent(f"""\
        Load command 7
                  cmd LC_VERSION_MIN_MACOSX
              cmdsize 16
              version {version}
                  sdk 14.0
    """)


class TestParseDeploymentVersions:
    def test_lc_build_version_minos(self):
        output = _lc_build_version_block("10.15")
        assert parse_deployment_versions(output) == ["10.15"]

    def test_lc_version_min_macosx(self):
        output = _lc_version_min_block("10.15")
        assert parse_deployment_versions(output) == ["10.15"]

    def test_both_forms(self):
        output = _lc_build_version_block("10.15") + _lc_version_min_block("10.15")
        result = parse_deployment_versions(output)
        assert sorted(result) == ["10.15", "10.15"]

    def test_three_part_version(self):
        output = _lc_build_version_block("10.15.0")
        assert parse_deployment_versions(output) == ["10.15.0"]

    def test_no_deployment_info(self):
        assert parse_deployment_versions("no deployment info here\n") == []

    def test_empty_string(self):
        assert parse_deployment_versions("") == []

    def test_wrong_version_detected(self):
        output = _lc_build_version_block("13.0")
        result = parse_deployment_versions(output)
        assert result == ["13.0"]


# ---------------------------------------------------------------------------
# parse_sck_link_type  — the most critical check for the 11.5.2 compatibility
# ---------------------------------------------------------------------------

def _dylib_block(cmd: str, name: str) -> str:
    return textwrap.dedent(f"""\
        Load command 99
                  cmd {cmd}
              cmdsize 80
                 name {name} (offset 24)
    """)


class TestParseSckLinkType:
    def test_strong_link(self):
        output = _dylib_block("LC_LOAD_DYLIB", "/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit")
        assert parse_sck_link_type(output) == "strong"

    def test_weak_link(self):
        output = _dylib_block("LC_LOAD_WEAK_DYLIB", "/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit")
        assert parse_sck_link_type(output) == "weak"

    def test_absent(self):
        output = _dylib_block("LC_LOAD_DYLIB", "/System/Library/Frameworks/Foundation.framework/Foundation")
        assert parse_sck_link_type(output) == "absent"

    def test_empty_input(self):
        assert parse_sck_link_type("") == "absent"

    def test_sck_in_unrelated_text_outside_block(self):
        # SCK mentioned in a comment or annotation not inside a Load command
        # block should not trigger a false positive.
        output = "# References to ScreenCaptureKit in notes\nsome other load commands\n"
        assert parse_sck_link_type(output) == "absent"

    def test_strong_takes_precedence_over_weak(self):
        # Both present (unusual but define expected behaviour).
        output = (
            _dylib_block("LC_LOAD_WEAK_DYLIB", "/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit")
            + _dylib_block("LC_LOAD_DYLIB", "/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit")
        )
        assert parse_sck_link_type(output) == "strong"

    def test_other_framework_plus_sck_weak(self):
        output = (
            _dylib_block("LC_LOAD_DYLIB", "/System/Library/Frameworks/Foundation.framework/Foundation")
            + _dylib_block("LC_LOAD_WEAK_DYLIB", "/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit")
        )
        assert parse_sck_link_type(output) == "weak"

    def test_real_world_otool_excerpt(self):
        # Realistic excerpt to catch regressions from actual otool output format.
        output = textwrap.dedent("""\
            /path/to/Wavis:
            Load command 0
                      cmd LC_SEGMENT_64
            Load command 1
                      cmd LC_LOAD_DYLIB
                  cmdsize 80
                     name /usr/lib/libSystem.B.dylib (offset 24)
            Load command 2
                      cmd LC_LOAD_WEAK_DYLIB
                  cmdsize 96
                     name /System/Library/Frameworks/ScreenCaptureKit.framework/Versions/A/ScreenCaptureKit (offset 24)
            Load command 3
                      cmd LC_LOAD_DYLIB
                  cmdsize 72
                     name /usr/lib/libobjc.A.dylib (offset 24)
        """)
        assert parse_sck_link_type(output) == "weak"


# ---------------------------------------------------------------------------
# parse_lipo_arches
# ---------------------------------------------------------------------------

class TestParseLipoArches:
    def test_fat_arm64_x86_64(self):
        output = "Architectures in the fat file: Wavis are: x86_64 arm64\n"
        assert parse_lipo_arches(output) == "x86_64 arm64"

    def test_non_fat_arm64(self):
        output = "Non-fat file: Wavis is architecture: arm64\n"
        assert parse_lipo_arches(output) == "arm64"

    def test_non_fat_x86_64(self):
        output = "Non-fat file: Wavis is architecture: x86_64\n"
        assert parse_lipo_arches(output) == "x86_64"

    def test_unrecognised_output(self):
        assert parse_lipo_arches("error: no such file\n") == ""

    def test_empty_output(self):
        assert parse_lipo_arches("") == ""


# ---------------------------------------------------------------------------
# normalize_arch
# ---------------------------------------------------------------------------

class TestNormalizeArch:
    def test_arm64_passthrough(self):
        assert normalize_arch("arm64") == "arm64"

    def test_x86_64_passthrough(self):
        assert normalize_arch("x86_64") == "x86_64"

    def test_amd64_alias(self):
        assert normalize_arch("amd64") == "x86_64"

    def test_x64_alias(self):
        assert normalize_arch("x64") == "x86_64"

    def test_aarch64_alias(self):
        assert normalize_arch("aarch64") == "arm64"

    def test_uppercase_normalised(self):
        assert normalize_arch("ARM64") == "arm64"

    def test_mixed_case_alias(self):
        assert normalize_arch("AMD64") == "x86_64"

    def test_leading_trailing_whitespace(self):
        assert normalize_arch("  arm64  ") == "arm64"


# ---------------------------------------------------------------------------
# load_machines
# ---------------------------------------------------------------------------

class TestLoadMachines:
    def _write_toml(self, tmp_path: Path, content: str) -> Path:
        p = tmp_path / "machines.toml"
        p.write_text(content, encoding="utf-8")
        return p

    def test_single_complete_entry(self, tmp_path):
        p = self._write_toml(tmp_path, textwrap.dedent("""\
            [[machines]]
            name    = "mac-ventura"
            host    = "192.168.1.42"
            user    = "ci"
            ssh_key = "~/.ssh/compat_rsa"
            arch    = "x86_64"
            macos   = "13.6"
            tiers   = ["t0", "t1", "t2"]
        """))
        machines = load_machines(p)
        assert len(machines) == 1
        m = machines[0]
        assert m.name == "mac-ventura"
        assert m.host == "192.168.1.42"
        assert m.user == "ci"
        assert m.arch == "x86_64"
        assert m.macos == "13.6"
        assert m.tiers == ("t0", "t1", "t2")

    def test_multiple_machines(self, tmp_path):
        p = self._write_toml(tmp_path, textwrap.dedent("""\
            [[machines]]
            name = "a"
            host = "1.1.1.1"
            user = "u"
            ssh_key = "k"

            [[machines]]
            name = "b"
            host = "2.2.2.2"
            user = "u"
            ssh_key = "k"
        """))
        machines = load_machines(p)
        assert [m.name for m in machines] == ["a", "b"]

    def test_missing_required_key_raises(self, tmp_path):
        # ssh_key is required.
        p = self._write_toml(tmp_path, textwrap.dedent("""\
            [[machines]]
            name = "m"
            host = "1.1.1.1"
            user = "ci"
        """))
        with pytest.raises(ValueError, match="ssh_key"):
            load_machines(p)

    def test_default_tiers_when_omitted(self, tmp_path):
        p = self._write_toml(tmp_path, textwrap.dedent("""\
            [[machines]]
            name = "m"
            host = "1.1.1.1"
            user = "u"
            ssh_key = "k"
        """))
        machines = load_machines(p)
        assert machines[0].tiers == ("t0", "t1")

    def test_target_property(self, tmp_path):
        p = self._write_toml(tmp_path, textwrap.dedent("""\
            [[machines]]
            name = "m"
            host = "10.0.0.1"
            user = "dev"
            ssh_key = "k"
        """))
        m = load_machines(p)[0]
        assert m.target == "dev@10.0.0.1"

    def test_empty_machines_list(self, tmp_path):
        p = self._write_toml(tmp_path, "# no machines\n")
        assert load_machines(p) == []
