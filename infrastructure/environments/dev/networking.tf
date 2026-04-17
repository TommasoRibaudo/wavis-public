# ---------- Private Subnet Migration — Networking ----------
#
# Creates a private subnet (for the backend) and a public NAT subnet
# (for the NAT gateway + LiveKit). All resources are gated behind
# var.enable_private_subnet so the current public-subnet topology is
# preserved until the operator explicitly opts in.
#
# Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 10.1

# ── Data: look up the existing internet gateway in the VPC ──────────

data "aws_internet_gateway" "existing" {
  count = var.enable_private_subnet ? 1 : 0

  filter {
    name   = "attachment.vpc-id"
    values = [data.aws_subnet.existing.vpc_id]
  }
}

# ── Subnets ─────────────────────────────────────────────────────────

resource "aws_subnet" "private" {
  count = var.enable_private_subnet ? 1 : 0

  vpc_id            = data.aws_subnet.existing.vpc_id
  availability_zone = data.aws_subnet.existing.availability_zone

  # Use explicit CIDR if provided, otherwise carve /24 blocks from the VPC CIDR.
  # cidrsubnet(vpc_cidr, newbits, netnum) — e.g. 172.31.0.0/16 → /24 with netnum 100
  cidr_block = (
    var.private_subnet_cidr != ""
    ? var.private_subnet_cidr
    : cidrsubnet(data.aws_vpc.existing.cidr_block, 8, 100)
  )

  map_public_ip_on_launch = false

  tags = merge(local.tags, {
    Name = "${local.project}-private-${local.env}"
  })
}

resource "aws_subnet" "public_nat" {
  count = var.enable_private_subnet ? 1 : 0

  vpc_id            = data.aws_subnet.existing.vpc_id
  availability_zone = data.aws_subnet.existing.availability_zone

  cidr_block = (
    var.public_nat_subnet_cidr != ""
    ? var.public_nat_subnet_cidr
    : cidrsubnet(data.aws_vpc.existing.cidr_block, 8, 101)
  )

  map_public_ip_on_launch = true

  tags = merge(local.tags, {
    Name = "${local.project}-public-nat-${local.env}"
  })
}

# ── NAT Gateway + Elastic IP ───────────────────────────────────────

resource "aws_eip" "nat" {
  count  = var.enable_private_subnet ? 1 : 0
  domain = "vpc"

  tags = merge(local.tags, {
    Name = "${local.project}-nat-eip-${local.env}"
  })
}

resource "aws_nat_gateway" "main" {
  count         = var.enable_private_subnet ? 1 : 0
  allocation_id = aws_eip.nat[0].id
  subnet_id     = aws_subnet.public_nat[0].id

  tags = merge(local.tags, {
    Name = "${local.project}-nat-${local.env}"
  })

  depends_on = [data.aws_internet_gateway.existing]
}

# ── Route Tables ───────────────────────────────────────────────────

# Private route table: default route → NAT gateway (outbound-only internet)
resource "aws_route_table" "private" {
  count  = var.enable_private_subnet ? 1 : 0
  vpc_id = data.aws_subnet.existing.vpc_id

  tags = merge(local.tags, {
    Name = "${local.project}-private-rt-${local.env}"
  })
}

resource "aws_route" "private_nat" {
  count                  = var.enable_private_subnet ? 1 : 0
  route_table_id         = aws_route_table.private[0].id
  destination_cidr_block = "0.0.0.0/0"
  nat_gateway_id         = aws_nat_gateway.main[0].id
}

# Public NAT route table: default route → internet gateway
resource "aws_route_table" "public_nat" {
  count  = var.enable_private_subnet ? 1 : 0
  vpc_id = data.aws_subnet.existing.vpc_id

  tags = merge(local.tags, {
    Name = "${local.project}-public-nat-rt-${local.env}"
  })
}

resource "aws_route" "public_nat_igw" {
  count                  = var.enable_private_subnet ? 1 : 0
  route_table_id         = aws_route_table.public_nat[0].id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = data.aws_internet_gateway.existing[0].id
}

# ── Route Table Associations ───────────────────────────────────────

resource "aws_route_table_association" "private" {
  count          = var.enable_private_subnet ? 1 : 0
  subnet_id      = aws_subnet.private[0].id
  route_table_id = aws_route_table.private[0].id
}

resource "aws_route_table_association" "public_nat" {
  count          = var.enable_private_subnet ? 1 : 0
  subnet_id      = aws_subnet.public_nat[0].id
  route_table_id = aws_route_table.public_nat[0].id
}
