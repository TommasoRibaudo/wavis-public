# ---------- Security Group (hardened for HTTPS from anywhere) ----------
#
# Attached to the EC2 instance via aws_instance.wavis.vpc_security_group_ids.

resource "aws_security_group" "wavis" {
  name_prefix = "${local.project}-${local.env}-"
  description = "Wavis dev - hardened (HTTPS from anywhere, SSH restricted)"
  vpc_id      = data.aws_subnet.existing.vpc_id
  tags        = merge(local.tags, { Name = "${local.project}-${local.env}-sg" })

  lifecycle {
    create_before_destroy = true
  }
}

# SSH — disabled in favor of SSM Session Manager.
# Uncomment if you need emergency SSH access.
# resource "aws_vpc_security_group_ingress_rule" "ssh" {
#   for_each          = toset(var.allowed_ssh_cidrs)
#   security_group_id = aws_security_group.wavis.id
#   cidr_ipv4         = each.value
#   from_port         = 22
#   to_port           = 22
#   ip_protocol       = "tcp"
#   description       = "SSH"
# }

# HTTPS (443) from anywhere — for CloudFront or direct TLS connections
resource "aws_vpc_security_group_ingress_rule" "https" {
  security_group_id = aws_security_group.wavis.id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 443
  to_port           = 443
  ip_protocol       = "tcp"
  description       = "HTTPS from anywhere"
}

# Backend HTTP (3000) — CloudFront origin pull only.
# When use_cf_prefix_list = true, restricts to CloudFront IP ranges via AWS-managed prefix list.
# When false, restricts to allowed_ssh_cidrs only (your IP).
# To also allow direct access for local testing, set var.allow_direct_backend = true.
resource "aws_vpc_security_group_ingress_rule" "backend_cf" {
  count             = var.use_cf_prefix_list ? 1 : 0
  security_group_id = aws_security_group.wavis.id
  prefix_list_id    = data.aws_ec2_managed_prefix_list.cloudfront[0].id
  from_port         = 3000
  to_port           = 3000
  ip_protocol       = "tcp"
  description       = "Backend HTTP + WebSocket (CloudFront origin only)"
}

# Fallback: when prefix list is unavailable, allow from your IP only
resource "aws_vpc_security_group_ingress_rule" "backend_ip" {
  for_each          = var.use_cf_prefix_list ? toset([]) : toset(var.allowed_ssh_cidrs)
  security_group_id = aws_security_group.wavis.id
  cidr_ipv4         = each.value
  from_port         = 3000
  to_port           = 3000
  ip_protocol       = "tcp"
  description       = "Backend HTTP + WebSocket (your IP fallback)"
}

# Optional: direct backend access from your IP for testing (disabled by default)
resource "aws_vpc_security_group_ingress_rule" "backend_direct" {
  count             = var.allow_direct_backend && var.use_cf_prefix_list ? 1 : 0
  security_group_id = aws_security_group.wavis.id
  cidr_ipv4         = var.allowed_ssh_cidrs[0]
  from_port         = 3000
  to_port           = 3000
  ip_protocol       = "tcp"
  description       = "Backend direct access (dev testing)"
}

# LiveKit WebSocket + HTTP API (7880)
# Open to the internet — LiveKit is token-protected. Using the CF prefix list
# here would consume ~60 additional SG rules (one per CIDR), exceeding the
# default 60-rule-per-SG quota. Backend port 3000 uses the prefix list instead
# since it's the primary attack surface.
resource "aws_vpc_security_group_ingress_rule" "livekit_ws" {
  count             = var.livekit_deployment_mode == "colocated" ? 1 : 0
  security_group_id = aws_security_group.wavis.id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 7880
  to_port           = 7880
  ip_protocol       = "tcp"
  description       = "LiveKit WebSocket + HTTP API (token-protected)"
}

# LiveKit ICE over TCP (7881) — direct client-to-SFU media, cannot be proxied.
# Must remain open to the internet. Protected by LiveKit token auth.
resource "aws_vpc_security_group_ingress_rule" "livekit_rtc_tcp" {
  count             = var.livekit_deployment_mode == "colocated" ? 1 : 0
  security_group_id = aws_security_group.wavis.id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 7881
  to_port           = 7881
  ip_protocol       = "tcp"
  description       = "LiveKit ICE over TCP (direct, token-protected)"
}

# LiveKit ICE media ports (UDP) — direct client-to-SFU media, cannot be proxied.
# Must remain open to the internet. Protected by LiveKit token auth.
resource "aws_vpc_security_group_ingress_rule" "livekit_ice_udp" {
  count             = var.livekit_deployment_mode == "colocated" ? 1 : 0
  security_group_id = aws_security_group.wavis.id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 50000
  to_port           = 50100
  ip_protocol       = "udp"
  description       = "LiveKit ICE media UDP (direct, token-protected)"
}

# All outbound
resource "aws_vpc_security_group_egress_rule" "all_out" {
  security_group_id = aws_security_group.wavis.id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "All outbound"
}
