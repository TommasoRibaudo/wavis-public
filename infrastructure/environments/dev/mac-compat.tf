# ---------- EC2 Mac Dedicated Hosts — macOS Compatibility Testing ----------
#
# Two independent Dedicated Hosts:
#
#   Intel host (mac1.metal) — one host, one instance at a time.
#     Runs either macOS 13 Ventura OR macOS 11 Big Sur depending on
#     mac_intel_target_macos. Swap between versions by changing the variable
#     and re-applying: Terraform terminates the old instance and launches a new
#     one on the same host. The host keeps billing continuously, so you pay the
#     24-hour minimum once regardless of how many AMI swaps you do that day.
#
#   ARM host (mac2.metal) — macOS 26 Tahoe (or configurable).
#
# Enable independently:
#   enable_mac_compat_intel = true   ~$1.08/hr, 24 h min (~$26)
#   enable_mac_compat_arm   = true   ~$0.65/hr, 24 h min (~$16)
#   Both                            ~$1.73/hr, 24 h min (~$42)
#
# Typical single-machine workflow (cheapest full Intel coverage):
#   1. terraform apply -var="enable_mac_compat_intel=true" \
#                      -var="mac_intel_target_macos=ventura"
#   2. python tools/compat/gen-machines-local.py   # writes machines.local.toml
#   3. python tools/compat/compat-run.py --app Wavis.app --machine mac-intel
#   4. Swap to Big Sur (same host, no extra billing):
#      terraform apply -var="enable_mac_compat_intel=true" \
#                      -var="mac_intel_target_macos=bigsur"
#      python tools/compat/gen-machines-local.py
#      python tools/compat/compat-run.py --app Wavis.app --machine mac-intel
#   5. Between runs within 24 h: stop instance, keep host (see gen-machines-local output).
#   6. End of day: terminate instance, destroy host (see README Step 8).
#
# AZ requirement:
#   Dedicated Hosts and their instances must be in the same AZ.
#   Set mac_compat_az and (if needed) mac_compat_subnet_id accordingly.

locals {
  intel_enabled = var.enable_mac_compat_intel
  arm_enabled   = var.enable_mac_compat_arm
  any_enabled   = local.intel_enabled || local.arm_enabled

  mac_compat_subnet = (
    var.mac_compat_subnet_id != "" ? var.mac_compat_subnet_id : var.subnet_id
  )
}

# ── AMI lookups ──────────────────────────────────────────────────────────────
# Each data source is only queried when its host is enabled and selected.

data "aws_ami" "mac_ventura" {
  count       = local.intel_enabled && var.mac_intel_target_macos == "ventura" ? 1 : 0
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["amzn-ec2-macos-13.*"]
  }

  filter {
    name   = "architecture"
    values = ["x86_64_mac"]
  }
}

data "aws_ami" "mac_bigsur" {
  count       = local.intel_enabled && var.mac_intel_target_macos == "bigsur" ? 1 : 0
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["amzn-ec2-macos-11.*"]
  }

  filter {
    name   = "architecture"
    values = ["x86_64_mac"]
  }
}

data "aws_ami" "mac_arm" {
  count       = local.arm_enabled && var.mac_arm_ami_id == "" ? 1 : 0
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = [var.mac_arm_ami_name_filter]
  }

  filter {
    name   = "architecture"
    values = ["arm64_mac"]
  }
}

locals {
  # Resolve which AMI the Intel instance uses based on mac_intel_target_macos.
  mac_intel_ami = (
    local.intel_enabled
    ? (var.mac_intel_target_macos == "ventura"
      ? data.aws_ami.mac_ventura[0].id
      : data.aws_ami.mac_bigsur[0].id)
    : ""
  )

  # Resolve ARM AMI: explicit override takes priority over data source lookup.
  mac_arm_ami_resolved = (
    var.mac_arm_ami_id != ""
    ? var.mac_arm_ami_id
    : (local.arm_enabled ? data.aws_ami.mac_arm[0].id : "")
  )

  # Machine name and tier list reflect whichever macOS is currently loaded.
  intel_machine_name = var.mac_intel_target_macos == "ventura" ? "mac-ventura-intel" : "mac-bigsur-intel"
  intel_macos_tag    = var.mac_intel_target_macos == "ventura" ? "13.x" : "11.x"
  intel_tiers        = var.mac_intel_target_macos == "ventura" ? "t0,t1,t2,t3" : "t0,t1,t2"
}

# ── Security Group ───────────────────────────────────────────────────────────

resource "aws_security_group" "mac_compat" {
  count       = local.any_enabled ? 1 : 0
  name_prefix = "${local.project}-mac-compat-"
  description = "macOS compat test hosts — SSH from controller IPs only"
  vpc_id      = data.aws_subnet.existing.vpc_id

  tags = merge(local.tags, { Name = "${local.project}-mac-compat-sg-${local.env}" })

  lifecycle { create_before_destroy = true }
}

resource "aws_vpc_security_group_ingress_rule" "mac_compat_ssh" {
  for_each          = local.any_enabled ? toset(var.allowed_ssh_cidrs) : toset([])
  security_group_id = aws_security_group.mac_compat[0].id
  cidr_ipv4         = each.value
  from_port         = 22
  to_port           = 22
  ip_protocol       = "tcp"
  description       = "SSH from compat-run controller"
}

resource "aws_vpc_security_group_egress_rule" "mac_compat_all_out" {
  count             = local.any_enabled ? 1 : 0
  security_group_id = aws_security_group.mac_compat[0].id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "All outbound"
}

# ── Intel Dedicated Host ─────────────────────────────────────────────────────
# Single host shared by Ventura and Big Sur runs. Swapping mac_intel_target_macos
# replaces the instance; the host (and its billing) is unaffected.

resource "aws_ec2_host" "compat_intel" {
  count             = local.intel_enabled ? 1 : 0
  instance_type     = "mac1.metal"
  availability_zone = var.mac_compat_az

  tags = merge(local.tags, {
    Name    = "${local.project}-compat-intel-${local.env}"
    Purpose = "compat-testing"
  })
}

resource "aws_instance" "mac_compat_intel" {
  count                  = local.intel_enabled ? 1 : 0
  ami                    = local.mac_intel_ami
  instance_type          = "mac1.metal"
  host_id                = aws_ec2_host.compat_intel[0].id
  subnet_id              = local.mac_compat_subnet
  key_name               = var.key_pair_name
  vpc_security_group_ids = [aws_security_group.mac_compat[0].id]

  root_block_device {
    volume_size           = 60
    volume_type           = "gp3"
    delete_on_termination = true
  }

  tags = merge(local.tags, {
    Name    = "${local.project}-compat-intel-${var.mac_intel_target_macos}"
    MacOS   = local.intel_macos_tag
    Arch    = "x86_64"
    Purpose = "compat-testing"
  })
}

# ── ARM Dedicated Host ───────────────────────────────────────────────────────

resource "aws_ec2_host" "compat_arm" {
  count             = local.arm_enabled ? 1 : 0
  instance_type     = "mac2.metal"
  availability_zone = var.mac_compat_az

  tags = merge(local.tags, {
    Name    = "${local.project}-compat-arm-${local.env}"
    Purpose = "compat-testing"
  })
}

resource "aws_instance" "mac_compat_arm" {
  count                  = local.arm_enabled ? 1 : 0
  ami                    = local.mac_arm_ami_resolved
  instance_type          = "mac2.metal"
  host_id                = aws_ec2_host.compat_arm[0].id
  subnet_id              = local.mac_compat_subnet
  key_name               = var.key_pair_name
  vpc_security_group_ids = [aws_security_group.mac_compat[0].id]

  root_block_device {
    volume_size           = 60
    volume_type           = "gp3"
    delete_on_termination = true
  }

  tags = merge(local.tags, {
    Name    = "${local.project}-compat-arm"
    MacOS   = "26.x"
    Arch    = "arm64"
    Purpose = "compat-testing"
  })
}
