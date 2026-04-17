#!/usr/bin/env bash
# setup-unattended-upgrades.sh — Install and configure automatic security updates.
# Targets Amazon Linux 2023 (dnf-automatic). Idempotent — safe to run multiple times.
#
# Usage: Run on the EC2 instance via SSM Session Manager:
#   aws ssm start-session --target <instance-id>
#   sudo bash ~/wavis/deploy/setup-unattended-upgrades.sh
#
# Requirements: 9.5

set -euo pipefail

echo "=== Setting up automatic security updates ==="

# ---------------------------------------------------------------------------
# Detect OS — this script targets Amazon Linux 2023 (dnf-based).
# Falls back to apt-based unattended-upgrades for Ubuntu/Debian if needed.
# ---------------------------------------------------------------------------
if command -v dnf &>/dev/null; then
  OS_FAMILY="dnf"
elif command -v apt-get &>/dev/null; then
  OS_FAMILY="apt"
else
  echo "ERROR: Unsupported OS — neither dnf nor apt found." >&2
  exit 1
fi

echo "Detected package manager: ${OS_FAMILY}"

# ---------------------------------------------------------------------------
# Amazon Linux 2023 / Fedora / RHEL 9+ — use dnf-automatic
# ---------------------------------------------------------------------------
if [[ "$OS_FAMILY" == "dnf" ]]; then
  echo "Installing dnf-automatic..."
  dnf install -y dnf-automatic

  # Configure dnf-automatic for security-only updates applied automatically.
  CONF="/etc/dnf/automatic.conf"
  echo "Configuring ${CONF}..."

  # apply_updates = yes  — actually install updates, don't just download
  sed -i 's/^apply_updates\s*=.*/apply_updates = yes/' "$CONF"

  # upgrade_type = security — only apply security updates
  sed -i 's/^upgrade_type\s*=.*/upgrade_type = security/' "$CONF"

  # Ensure download_updates is enabled (downloads before applying)
  sed -i 's/^download_updates\s*=.*/download_updates = yes/' "$CONF"

  # Enable and start the systemd timer for daily execution
  echo "Enabling dnf-automatic-install.timer..."
  systemctl enable --now dnf-automatic-install.timer

  echo "Verifying timer is active..."
  systemctl is-active dnf-automatic-install.timer
  systemctl status dnf-automatic-install.timer --no-pager || true

  echo "=== dnf-automatic configured for daily security updates ==="

# ---------------------------------------------------------------------------
# Ubuntu / Debian — use unattended-upgrades
# ---------------------------------------------------------------------------
elif [[ "$OS_FAMILY" == "apt" ]]; then
  echo "Installing unattended-upgrades..."
  DEBIAN_FRONTEND=noninteractive apt-get update -y
  DEBIAN_FRONTEND=noninteractive apt-get install -y unattended-upgrades

  # Enable the unattended-upgrades periodic config
  cat > /etc/apt/apt.conf.d/20auto-upgrades <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
APT::Periodic::AutocleanInterval "7";
EOF

  echo "Enabling unattended-upgrades service..."
  systemctl enable --now unattended-upgrades

  echo "=== unattended-upgrades configured for daily security updates ==="
fi

echo "Done. Automatic security updates are now enabled."
