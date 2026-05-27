resource "aws_vpc" "main" {
  cidr_block           = var.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-vpc"
  })
}

resource "aws_internet_gateway" "main" {
  vpc_id = aws_vpc.main.id

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-igw"
  })
}

resource "aws_subnet" "public" {
  for_each = {
    for idx, az in local.azs : az => {
      cidr = var.public_subnet_cidrs[idx]
      az   = az
    }
  }

  vpc_id                  = aws_vpc.main.id
  availability_zone       = each.value.az
  cidr_block              = each.value.cidr
  map_public_ip_on_launch = true

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-public-${each.value.az}"
    Tier = "public"
  })
}

resource "aws_subnet" "private_app" {
  for_each = {
    for idx, az in local.azs : az => {
      cidr = var.private_app_subnet_cidrs[idx]
      az   = az
    }
  }

  vpc_id            = aws_vpc.main.id
  availability_zone = each.value.az
  cidr_block        = each.value.cidr

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-private-app-${each.value.az}"
    Tier = "private-app"
  })
}

resource "aws_subnet" "private_data" {
  for_each = {
    for idx, az in local.azs : az => {
      cidr = var.private_data_subnet_cidrs[idx]
      az   = az
    }
  }

  vpc_id            = aws_vpc.main.id
  availability_zone = each.value.az
  cidr_block        = each.value.cidr

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-private-data-${each.value.az}"
    Tier = "private-data"
  })
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.main.id

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-public-rt"
  })
}

resource "aws_route" "public_internet" {
  route_table_id         = aws_route_table.public.id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.main.id
}

resource "aws_route_table_association" "public" {
  for_each = aws_subnet.public

  subnet_id      = each.value.id
  route_table_id = aws_route_table.public.id
}

# Private route table is intentionally retained with no 0.0.0.0/0 route.
# In dev we run Fargate tasks in public subnets to avoid NAT Gateway costs
# (~$33/mo). The empty private_app / private_data subnets and this route
# table cost nothing on their own and let us re-introduce a NAT Gateway by
# adding back aws_eip.nat, aws_nat_gateway.main, and aws_route.private_nat.
# See doc/aws_costs/runbook.md (Tier 3) for the rationale.
resource "aws_route_table" "private" {
  vpc_id = aws_vpc.main.id

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-private-rt"
  })
}

resource "aws_route_table_association" "private_app" {
  for_each = aws_subnet.private_app

  subnet_id      = each.value.id
  route_table_id = aws_route_table.private.id
}

resource "aws_route_table_association" "private_data" {
  for_each = aws_subnet.private_data

  subnet_id      = each.value.id
  route_table_id = aws_route_table.private.id
}
