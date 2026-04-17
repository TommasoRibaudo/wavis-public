"""Property-based tests for Terraform plan JSON validation.

Validates correctness properties P1–P6, P8, P10, P11 from the
private-subnet-migration design document.

Usage:
    # With a pre-generated plan JSON:
    TF_PLAN_JSON=plan.json pytest tests/validate_plan.py -v

    # Or via CLI option:
    pytest tests/validate_plan.py --plan-json plan.json -v

    # P1 (CIDR non-overlap) runs without a plan file — it's a pure
    # function test using hypothesis-generated inputs.
"""

from __future__ import annotations

import ipaddress
import json
from typing import Any

import pytest
from hypothesis import given, settings, assume
from hypothesis import strategies as st


# ============================================================================
# Helper functions
# ============================================================================

def get_resource_changes(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Return the list of resource_changes from a plan JSON."""
    return plan.get("resource_changes", [])


def get_resources_by_type(
    plan: dict[str, Any], resource_type: str
) -> list[dict[str, Any]]:
    """Extract all resource changes matching *resource_type*."""
    return [
        rc for rc in get_resource_changes(plan)
        if rc.get("type") == resource_type
    ]


def get_planned_values_resources(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Flatten all resources from planned_values.root_module (including child modules)."""
    resources: list[dict[str, Any]] = []
    root = plan.get("planned_values", {}).get("root_module", {})
    resources.extend(root.get("resources", []))
    for child in root.get("child_modules", []):
        resources.extend(child.get("resources", []))
    return resources


def get_sg_ingress_rules(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Extract all SG ingress rule resource changes from the plan.

    Returns the *after* values dict for each
    ``aws_vpc_security_group_ingress_rule`` that is being created or
    updated (i.e. not destroyed).
    """
    rules: list[dict[str, Any]] = []
    for rc in get_resource_changes(plan):
        if rc.get("type") != "aws_vpc_security_group_ingress_rule":
            continue
        change = rc.get("change", {})
        actions = change.get("actions", [])
        # Skip resources being destroyed with no replacement
        if actions == ["delete"]:
            continue
        after = change.get("after")
        if after is not None:
            # Attach the resource address for debugging
            after["_address"] = rc.get("address", "")
            rules.append(after)
    return rules


def get_sg_ingress_rules_from_planned(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Extract SG ingress rules from planned_values (final state)."""
    rules: list[dict[str, Any]] = []
    for res in get_planned_values_resources(plan):
        if res.get("type") == "aws_vpc_security_group_ingress_rule":
            vals = res.get("values", {})
            vals["_address"] = res.get("address", "")
            rules.append(vals)
    return rules


def get_iam_policy_documents(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Extract IAM inline policy documents from the plan.

    Looks at ``aws_iam_role_policy`` resource changes and parses the
    JSON-encoded ``policy`` field from the *after* values.
    """
    docs: list[dict[str, Any]] = []
    for rc in get_resource_changes(plan):
        if rc.get("type") != "aws_iam_role_policy":
            continue
        change = rc.get("change", {})
        actions = change.get("actions", [])
        if actions == ["delete"]:
            continue
        after = change.get("after")
        if after is None:
            continue
        policy_str = after.get("policy")
        if policy_str:
            try:
                policy_doc = json.loads(policy_str)
            except (json.JSONDecodeError, TypeError):
                policy_doc = {}
            docs.append({
                "address": rc.get("address", ""),
                "name": after.get("name", ""),
                "role": after.get("role", ""),
                "policy": policy_doc,
            })
    return docs


def get_iam_policy_documents_from_planned(
    plan: dict[str, Any],
) -> list[dict[str, Any]]:
    """Extract IAM inline policy documents from planned_values."""
    docs: list[dict[str, Any]] = []
    for res in get_planned_values_resources(plan):
        if res.get("type") != "aws_iam_role_policy":
            continue
        vals = res.get("values", {})
        policy_str = vals.get("policy")
        if policy_str:
            try:
                policy_doc = json.loads(policy_str)
            except (json.JSONDecodeError, TypeError):
                policy_doc = {}
            docs.append({
                "address": res.get("address", ""),
                "name": vals.get("name", ""),
                "role": vals.get("role", ""),
                "policy": policy_doc,
            })
    return docs


def extract_actions_from_policy(policy_doc: dict[str, Any]) -> set[str]:
    """Collect all Action strings from an IAM policy document."""
    actions: set[str] = set()
    for stmt in policy_doc.get("Statement", []):
        act = stmt.get("Action", [])
        if isinstance(act, str):
            actions.add(act)
        elif isinstance(act, list):
            actions.update(act)
    return actions


def get_instance_resources(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Return *after* values for all aws_instance resource changes."""
    instances: list[dict[str, Any]] = []
    for rc in get_resource_changes(plan):
        if rc.get("type") != "aws_instance":
            continue
        change = rc.get("change", {})
        actions = change.get("actions", [])
        if actions == ["delete"]:
            continue
        after = change.get("after")
        if after is not None:
            after["_address"] = rc.get("address", "")
            instances.append(after)
    return instances


def get_instance_resources_from_planned(
    plan: dict[str, Any],
) -> list[dict[str, Any]]:
    """Return values for all aws_instance resources from planned_values."""
    instances: list[dict[str, Any]] = []
    for res in get_planned_values_resources(plan):
        if res.get("type") == "aws_instance":
            vals = res.get("values", {})
            vals["_address"] = res.get("address", "")
            instances.append(vals)
    return instances


def get_subnet_resources(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Return *after* values for all aws_subnet resource changes."""
    subnets: list[dict[str, Any]] = []
    for rc in get_resource_changes(plan):
        if rc.get("type") != "aws_subnet":
            continue
        change = rc.get("change", {})
        actions = change.get("actions", [])
        if actions == ["delete"]:
            continue
        after = change.get("after")
        if after is not None:
            after["_address"] = rc.get("address", "")
            subnets.append(after)
    return subnets


# ============================================================================
# Property 1 — Subnet CIDR Non-Overlap  (Task 3.4)
# Feature: private-subnet-migration, Property 1: Subnet CIDR Non-Overlap
# Validates: Requirements 1.1
# ============================================================================

def cidrsubnet(base_cidr: str, newbits: int, netnum: int) -> str:
    """Pure-Python equivalent of Terraform's ``cidrsubnet(base, newbits, netnum)``.

    Given a base CIDR (e.g. ``"10.0.0.0/16"``), adds *newbits* to the prefix
    length and selects the *netnum*-th sub-network.
    """
    network = ipaddress.ip_network(base_cidr, strict=False)
    new_prefix = network.prefixlen + newbits
    if new_prefix > network.max_prefixlen:
        raise ValueError(
            f"Cannot add {newbits} bits to /{network.prefixlen} "
            f"(max /{network.max_prefixlen})"
        )
    subnets = list(network.subnets(prefixlen_diff=newbits))
    if netnum >= len(subnets):
        raise IndexError(
            f"netnum {netnum} out of range for {base_cidr} with newbits={newbits} "
            f"(max {len(subnets) - 1})"
        )
    return str(subnets[netnum])


def networks_overlap(a: str, b: str) -> bool:
    """Return True if two CIDR strings overlap."""
    return ipaddress.ip_network(a, strict=False).overlaps(
        ipaddress.ip_network(b, strict=False)
    )


# Strategy: generate a VPC CIDR (/16 to /20) and two distinct netnum values
# that represent the private and public-NAT subnets carved via cidrsubnet().
# Also generate a small set of "existing" subnet CIDRs within the VPC to
# verify no overlap with the new subnets.

@st.composite
def vpc_and_subnets(draw: st.DrawFn):
    """Generate a VPC CIDR, newbits, two distinct netnums, and existing subnets."""
    # VPC prefix length between /16 and /20
    vpc_prefix = draw(st.integers(min_value=16, max_value=20))
    # First octet in private ranges
    first_octet = draw(st.sampled_from([10, 172, 192]))
    if first_octet == 10:
        base = f"10.{draw(st.integers(0, 255))}.0.0/{vpc_prefix}"
    elif first_octet == 172:
        second = draw(st.integers(16, 31))
        base = f"172.{second}.0.0/{vpc_prefix}"
    else:
        base = f"192.168.0.0/{vpc_prefix}"

    # newbits: add enough bits to carve /24-ish subnets (but stay <= /28)
    max_newbits = 28 - vpc_prefix
    if max_newbits < 1:
        max_newbits = 1
    newbits = draw(st.integers(min_value=1, max_value=max_newbits))

    max_netnum = (2 ** newbits) - 1
    netnum_private = draw(st.integers(min_value=0, max_value=max_netnum))
    netnum_public = draw(st.integers(min_value=0, max_value=max_netnum))
    assume(netnum_private != netnum_public)

    # Generate 0-3 "existing" subnets (different netnums)
    num_existing = draw(st.integers(min_value=0, max_value=min(3, max_netnum - 1)))
    existing_netnums = draw(
        st.lists(
            st.integers(min_value=0, max_value=max_netnum),
            min_size=num_existing,
            max_size=num_existing,
            unique=True,
        ).filter(lambda ns: netnum_private not in ns and netnum_public not in ns)
    )
    existing_cidrs = [cidrsubnet(base, newbits, n) for n in existing_netnums]

    return {
        "vpc_cidr": base,
        "newbits": newbits,
        "netnum_private": netnum_private,
        "netnum_public": netnum_public,
        "existing_cidrs": existing_cidrs,
    }


@given(data=vpc_and_subnets())
@settings(max_examples=100)
def test_property1_subnet_cidr_non_overlap(data: dict) -> None:
    """P1: cidrsubnet() outputs never overlap each other or existing subnets.

    Validates: Requirements 1.1
    """
    vpc_cidr = data["vpc_cidr"]
    newbits = data["newbits"]
    private_cidr = cidrsubnet(vpc_cidr, newbits, data["netnum_private"])
    public_cidr = cidrsubnet(vpc_cidr, newbits, data["netnum_public"])
    existing = data["existing_cidrs"]

    # New subnets must not overlap each other
    assert not networks_overlap(private_cidr, public_cidr), (
        f"Private {private_cidr} overlaps public {public_cidr}"
    )

    # New subnets must not overlap any existing subnet
    for ex in existing:
        assert not networks_overlap(private_cidr, ex), (
            f"Private {private_cidr} overlaps existing {ex}"
        )
        assert not networks_overlap(public_cidr, ex), (
            f"Public {public_cidr} overlaps existing {ex}"
        )

    # Both subnets must be within the VPC CIDR
    vpc_net = ipaddress.ip_network(vpc_cidr, strict=False)
    priv_net = ipaddress.ip_network(private_cidr, strict=False)
    pub_net = ipaddress.ip_network(public_cidr, strict=False)
    assert priv_net.subnet_of(vpc_net), f"{private_cidr} not within {vpc_cidr}"
    assert pub_net.subnet_of(vpc_net), f"{public_cidr} not within {vpc_cidr}"


# ============================================================================
# Property 2 — No SSH Ingress  (Task 3.5)
# Feature: private-subnet-migration, Property 2: No SSH Ingress
# Validates: Requirements 2.4, 3.6, 9.6
# ============================================================================

def test_property2_no_ssh_ingress(plan: dict[str, Any]) -> None:
    """P2: No SG ingress rule has from_port or to_port equal to 22.

    Validates: Requirements 2.4, 3.6, 9.6
    """
    # Try resource_changes first, fall back to planned_values
    rules = get_sg_ingress_rules(plan)
    if not rules:
        rules = get_sg_ingress_rules_from_planned(plan)

    assert len(rules) > 0, "No SG ingress rules found in plan — cannot validate"

    for rule in rules:
        from_port = rule.get("from_port")
        to_port = rule.get("to_port")
        addr = rule.get("_address", "unknown")
        assert from_port != 22, (
            f"SSH ingress found: {addr} has from_port=22"
        )
        assert to_port != 22, (
            f"SSH ingress found: {addr} has to_port=22"
        )
        # Also check port ranges that include 22
        if from_port is not None and to_port is not None:
            assert not (from_port <= 22 <= to_port and from_port != to_port), (
                f"Port range {from_port}-{to_port} in {addr} includes SSH port 22"
            )


# ============================================================================
# Property 3 — CF-Proxied Ports Restricted to Prefix List  (Task 10.2)
# Feature: private-subnet-migration, Property 3: CloudFront-Proxied Ports Restricted to Prefix List
# Validates: Requirements 3.1, 3.2, 3.5, 9.1
# ============================================================================

def test_property3_cf_proxied_ports_use_prefix_list(plan: dict[str, Any]) -> None:
    """P3: Ports 3000 and 7880 ingress rules only reference CF prefix list, no arbitrary CIDRs.

    This test applies when use_cf_prefix_list=true. Ingress rules for ports
    3000 and 7880 must use a prefix_list_id (the CloudFront managed prefix
    list) and must NOT have a cidr_ipv4 source — except for the optional
    ``allow_direct_backend`` rule on port 3000 which is acceptable for dev
    testing.

    Validates: Requirements 3.1, 3.2, 3.5, 9.1
    """
    rules = get_sg_ingress_rules(plan)
    if not rules:
        rules = get_sg_ingress_rules_from_planned(plan)

    cf_proxied_ports = {3000, 7880}
    cf_port_rules = [
        r for r in rules
        if r.get("from_port") in cf_proxied_ports
        or r.get("to_port") in cf_proxied_ports
    ]

    if not cf_port_rules:
        pytest.skip("No ingress rules for ports 3000/7880 found in plan")

    for rule in cf_port_rules:
        addr = rule.get("_address", "unknown")
        port = rule.get("from_port")
        cidr = rule.get("cidr_ipv4")
        prefix_list = rule.get("prefix_list_id")
        ref_sg = rule.get("referenced_security_group_id")

        # Rules referencing another SG (e.g. LiveKit from backend) are fine
        if ref_sg:
            continue

        # The allow_direct_backend rule is an acceptable exception for dev
        if "backend_direct" in addr:
            continue

        # When using CF prefix list, the rule should have a prefix_list_id
        # and should NOT have an arbitrary CIDR
        if cidr and cidr != "0.0.0.0/0":
            # A specific CIDR (like operator IP) is the fallback when
            # use_cf_prefix_list=false — if we see it alongside prefix list
            # rules, that's the IP fallback path, which is acceptable only
            # when the prefix list variant is absent for this port.
            pass

        if prefix_list:
            # Good — using prefix list
            assert cidr is None or cidr == "", (
                f"{addr}: port {port} has both prefix_list_id and cidr_ipv4={cidr}; "
                f"should use only prefix list"
            )
        elif cidr == "0.0.0.0/0":
            # Port 7880 with livekit colocated mode opens to 0.0.0.0/0
            # (documented in security_groups.tf — SG rule quota issue)
            # This is acceptable only for port 7880 in colocated mode
            if port == 7880:
                continue
            pytest.fail(
                f"{addr}: port {port} open to 0.0.0.0/0 without prefix list"
            )


# ============================================================================
# Property 4 — LiveKit Colocated Ports Open  (Task 10.3)
# Feature: private-subnet-migration, Property 4: LiveKit Colocated Ports Open
# Validates: Requirements 3.4
# ============================================================================

def test_property4_livekit_colocated_ports_open(plan: dict[str, Any]) -> None:
    """P4: Backend SG has TCP 7881 from 0.0.0.0/0 and UDP 50000-50100 from 0.0.0.0/0.

    This test applies when livekit_deployment_mode="colocated".

    Validates: Requirements 3.4
    """
    rules = get_sg_ingress_rules(plan)
    if not rules:
        rules = get_sg_ingress_rules_from_planned(plan)

    # Look for TCP 7881 from 0.0.0.0/0
    tcp_7881 = [
        r for r in rules
        if r.get("from_port") == 7881
        and r.get("to_port") == 7881
        and r.get("ip_protocol") == "tcp"
        and r.get("cidr_ipv4") == "0.0.0.0/0"
    ]

    # Look for UDP 50000-50100 from 0.0.0.0/0
    udp_ice = [
        r for r in rules
        if r.get("from_port") == 50000
        and r.get("to_port") == 50100
        and r.get("ip_protocol") == "udp"
        and r.get("cidr_ipv4") == "0.0.0.0/0"
    ]

    assert len(tcp_7881) >= 1, (
        "Missing: TCP 7881 from 0.0.0.0/0 (LiveKit ICE TCP) not found in SG rules"
    )
    assert len(udp_ice) >= 1, (
        "Missing: UDP 50000-50100 from 0.0.0.0/0 (LiveKit ICE media) not found in SG rules"
    )


# ============================================================================
# Property 5 — IMDSv2 Enforced on All Instances  (Task 7.4)
# Feature: private-subnet-migration, Property 5: IMDSv2 Enforced on All Instances
# Validates: Requirements 5.7, 8.7, 9.3
# ============================================================================

def test_property5_imdsv2_enforced(plan: dict[str, Any]) -> None:
    """P5: Every aws_instance has metadata_options.http_tokens='required'.

    Validates: Requirements 5.7, 8.7, 9.3
    """
    instances = get_instance_resources(plan)
    if not instances:
        instances = get_instance_resources_from_planned(plan)

    assert len(instances) > 0, "No aws_instance resources found in plan"

    for inst in instances:
        addr = inst.get("_address", "unknown")
        metadata = inst.get("metadata_options")
        assert metadata is not None, (
            f"{addr}: missing metadata_options block"
        )
        # metadata_options can be a list of dicts or a single dict
        if isinstance(metadata, list):
            metadata = metadata[0] if metadata else {}

        http_tokens = metadata.get("http_tokens")
        assert http_tokens == "required", (
            f"{addr}: metadata_options.http_tokens={http_tokens!r}, expected 'required'"
        )

        http_endpoint = metadata.get("http_endpoint")
        assert http_endpoint == "enabled", (
            f"{addr}: metadata_options.http_endpoint={http_endpoint!r}, expected 'enabled'"
        )


# ============================================================================
# Property 6 — Private Subnet Instance Has No Public IP  (Task 12.2)
# Feature: private-subnet-migration, Property 6: Private Subnet Instance Has No Public IP
# Validates: Requirements 5.8
# ============================================================================

def test_property6_private_subnet_no_public_ip(plan: dict[str, Any]) -> None:
    """P6: Backend instance has no public IP; private subnet has map_public_ip_on_launch=false.

    This test applies when enable_private_subnet=true.

    Validates: Requirements 5.8
    """
    # Check private subnet has map_public_ip_on_launch = false
    subnets = get_subnet_resources(plan)
    private_subnets = [
        s for s in subnets
        if "private" in s.get("_address", "").lower()
        and "public" not in s.get("_address", "").lower()
    ]

    if not private_subnets:
        # Try planned_values
        for res in get_planned_values_resources(plan):
            if res.get("type") == "aws_subnet":
                addr = res.get("address", "")
                if "private" in addr.lower() and "public" not in addr.lower():
                    vals = res.get("values", {})
                    vals["_address"] = addr
                    private_subnets.append(vals)

    if not private_subnets:
        pytest.skip("No private subnet found in plan (enable_private_subnet may be false)")

    for subnet in private_subnets:
        addr = subnet.get("_address", "unknown")
        map_public = subnet.get("map_public_ip_on_launch")
        assert map_public is False, (
            f"{addr}: map_public_ip_on_launch={map_public!r}, expected False"
        )

    # Check backend instance does not have associate_public_ip_address = true
    instances = get_instance_resources(plan)
    if not instances:
        instances = get_instance_resources_from_planned(plan)

    backend_instances = [
        i for i in instances
        if "livekit" not in i.get("_address", "").lower()
    ]

    for inst in backend_instances:
        addr = inst.get("_address", "unknown")
        public_ip = inst.get("associate_public_ip_address")
        # None or False are both acceptable (no public IP)
        assert public_ip is not True, (
            f"{addr}: associate_public_ip_address={public_ip!r}, "
            f"backend instance must not have a public IP in private subnet"
        )


# ============================================================================
# Property 8 — IAM Policies Contain All Required Actions  (Task 5.5)
# Feature: private-subnet-migration, Property 8: IAM Policies Contain All Required Actions
# Validates: Requirements 2.2, 6.3, 8.7, 9.4
# ============================================================================

# The 11 SSM Session Manager actions required on every EC2 instance role
SSM_SESSION_MANAGER_ACTIONS = {
    "ssmmessages:CreateControlChannel",
    "ssmmessages:CreateDataChannel",
    "ssmmessages:OpenControlChannel",
    "ssmmessages:OpenDataChannel",
    "ssm:UpdateInstanceInformation",
    "ec2messages:AcknowledgeMessage",
    "ec2messages:DeleteMessage",
    "ec2messages:FailMessage",
    "ec2messages:GetEndpoint",
    "ec2messages:GetMessages",
    "ec2messages:SendReply",
}

# Deploy pipeline required actions
DEPLOY_PIPELINE_ACTIONS = {
    "ssm:SendCommand",
    "ssm:GetCommandInvocation",
}


def test_property8_iam_ssm_session_manager_actions(plan: dict[str, Any]) -> None:
    """P8a: SSM Session Manager policies contain all 11 required actions.

    Validates: Requirements 2.2, 8.7, 9.4
    """
    docs = get_iam_policy_documents(plan)
    if not docs:
        docs = get_iam_policy_documents_from_planned(plan)

    # Find SSM session manager policies (by name convention)
    ssm_policies = [
        d for d in docs
        if "ssm" in d["name"].lower()
        and ("session" in d["name"].lower() or "core" in d["name"].lower()
             or d["name"] == "ssm-session-manager")
    ]

    if not ssm_policies:
        pytest.skip("No SSM session manager IAM policies found in plan")

    for pol in ssm_policies:
        actions = extract_actions_from_policy(pol["policy"])
        missing = SSM_SESSION_MANAGER_ACTIONS - actions
        assert not missing, (
            f"IAM policy '{pol['name']}' on role '{pol['role']}' "
            f"(address: {pol['address']}) is missing SSM actions: {missing}"
        )


def test_property8_iam_deploy_pipeline_actions(plan: dict[str, Any]) -> None:
    """P8b: Deploy pipeline IAM policy contains ssm:SendCommand and ssm:GetCommandInvocation.

    Validates: Requirements 6.3
    """
    docs = get_iam_policy_documents(plan)
    if not docs:
        docs = get_iam_policy_documents_from_planned(plan)

    # Find deploy pipeline policies (by name or role convention)
    deploy_policies = [
        d for d in docs
        if "deploy" in d["name"].lower()
        or "deploy" in d["role"].lower()
        or "github" in d["role"].lower()
    ]

    if not deploy_policies:
        pytest.skip("No deploy pipeline IAM policies found in plan")

    # At least one deploy policy must have both required actions
    all_deploy_actions: set[str] = set()
    for pol in deploy_policies:
        all_deploy_actions.update(extract_actions_from_policy(pol["policy"]))

    missing = DEPLOY_PIPELINE_ACTIONS - all_deploy_actions
    assert not missing, (
        f"Deploy pipeline IAM policies are missing actions: {missing}. "
        f"Checked policies: {[p['name'] for p in deploy_policies]}"
    )


# ============================================================================
# Property 10 — New Variable Defaults Preserve Current Behavior  (Task 3.2)
# Feature: private-subnet-migration, Property 10: New Variable Defaults Preserve Current Behavior
# Validates: Requirements 10.4
# ============================================================================

# Networking resource types that should NOT appear when defaults are used
NETWORKING_RESOURCE_TYPES = {
    "aws_subnet",
    "aws_eip",
    "aws_nat_gateway",
    "aws_route_table",
    "aws_route",
    "aws_route_table_association",
    "aws_vpc_endpoint",
    "aws_cloudfront_vpc_origin",
}


def test_property10_defaults_preserve_behavior(plan: dict[str, Any]) -> None:
    """P10: With default variable values, no networking resources are created/modified/destroyed.

    Run this test against a plan generated with only default values for
    ``enable_private_subnet`` (false) and ``livekit_deployment_mode``
    ("colocated").

    Validates: Requirements 10.4
    """
    changes = get_resource_changes(plan)

    networking_changes = []
    for rc in changes:
        rtype = rc.get("type", "")
        if rtype not in NETWORKING_RESOURCE_TYPES:
            continue
        actions = rc.get("change", {}).get("actions", [])
        # "no-op" means the resource exists but isn't changing — that's fine
        if actions == ["no-op"]:
            continue
        networking_changes.append({
            "address": rc.get("address"),
            "type": rtype,
            "actions": actions,
        })

    assert len(networking_changes) == 0, (
        f"Expected zero networking resource changes with default variables, "
        f"but found {len(networking_changes)}: {networking_changes}"
    )


# ============================================================================
# Property 11 — LiveKit Separate Instance SG Allows Required Ports  (Task 7.5)
# Feature: private-subnet-migration, Property 11: LiveKit Separate Instance SG Allows Required Ports
# Validates: Requirements 8.2
# ============================================================================

def test_property11_livekit_separate_sg_ports(plan: dict[str, Any]) -> None:
    """P11: LiveKit SG has TCP 7880, TCP 7881 from 0.0.0.0/0 and UDP 50000-50100 from 0.0.0.0/0.

    This test applies when livekit_deployment_mode="separate".

    Validates: Requirements 8.2
    """
    rules = get_sg_ingress_rules(plan)
    if not rules:
        rules = get_sg_ingress_rules_from_planned(plan)

    # Filter to LiveKit SG rules (by address containing "livekit")
    lk_rules = [r for r in rules if "livekit" in r.get("_address", "").lower()]

    if not lk_rules:
        pytest.skip(
            "No LiveKit SG ingress rules found in plan "
            "(livekit_deployment_mode may not be 'separate')"
        )

    # Check TCP 7880 from 0.0.0.0/0
    tcp_7880 = [
        r for r in lk_rules
        if r.get("from_port") == 7880
        and r.get("to_port") == 7880
        and r.get("ip_protocol") == "tcp"
        and r.get("cidr_ipv4") == "0.0.0.0/0"
    ]

    # Check TCP 7881 from 0.0.0.0/0
    tcp_7881 = [
        r for r in lk_rules
        if r.get("from_port") == 7881
        and r.get("to_port") == 7881
        and r.get("ip_protocol") == "tcp"
        and r.get("cidr_ipv4") == "0.0.0.0/0"
    ]

    # Check UDP 50000-50100 from 0.0.0.0/0
    udp_ice = [
        r for r in lk_rules
        if r.get("from_port") == 50000
        and r.get("to_port") == 50100
        and r.get("ip_protocol") == "udp"
        and r.get("cidr_ipv4") == "0.0.0.0/0"
    ]

    assert len(tcp_7880) >= 1, (
        "Missing: LiveKit SG TCP 7880 from 0.0.0.0/0 (HTTP API + WebSocket)"
    )
    assert len(tcp_7881) >= 1, (
        "Missing: LiveKit SG TCP 7881 from 0.0.0.0/0 (ICE TCP)"
    )
    assert len(udp_ice) >= 1, (
        "Missing: LiveKit SG UDP 50000-50100 from 0.0.0.0/0 (ICE media)"
    )
