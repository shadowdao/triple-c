# The network, by network_mode:
#   public       new VPC, two public subnets, internet gateway; tasks get a
#                public IP (the security group still admits nothing).
#   private-nat  the same plus two private subnets behind one NAT gateway;
#                tasks run in the private subnets without a public IP.
#   existing     the given vpc_id / subnet_ids; nothing is created here.

locals {
  create_vpc  = var.network_mode != "existing"
  private_nat = var.network_mode == "private-nat"
  vpc_cidr    = "10.42.0.0/16"

  vpc_id = local.create_vpc ? one(aws_vpc.this[*].id) : var.vpc_id
  task_subnet_ids = (
    var.network_mode == "existing" ? var.subnet_ids
    : local.private_nat ? aws_subnet.private[*].id
    : aws_subnet.public[*].id
  )
  assign_public_ip = (
    var.network_mode == "existing" ? var.assign_public_ip
    : var.network_mode == "public"
  )
}

data "aws_availability_zones" "available" {
  count = local.create_vpc ? 1 : 0
  state = "available"

  filter {
    name   = "opt-in-status"
    values = ["opt-in-not-required"]
  }
}

resource "aws_vpc" "this" {
  count                = local.create_vpc ? 1 : 0
  cidr_block           = local.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true

  tags = { Name = local.name }
}

resource "aws_internet_gateway" "this" {
  count  = local.create_vpc ? 1 : 0
  vpc_id = aws_vpc.this[0].id

  tags = { Name = local.name }
}

resource "aws_subnet" "public" {
  count             = local.create_vpc ? 2 : 0
  vpc_id            = aws_vpc.this[0].id
  cidr_block        = cidrsubnet(local.vpc_cidr, 8, count.index)
  availability_zone = data.aws_availability_zones.available[0].names[count.index]

  tags = { Name = "${local.name}-public-${count.index}" }
}

resource "aws_route_table" "public" {
  count  = local.create_vpc ? 1 : 0
  vpc_id = aws_vpc.this[0].id

  tags = { Name = "${local.name}-public" }
}

resource "aws_route" "public_internet" {
  count                  = local.create_vpc ? 1 : 0
  route_table_id         = aws_route_table.public[0].id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.this[0].id
}

resource "aws_route_table_association" "public" {
  count          = local.create_vpc ? 2 : 0
  subnet_id      = aws_subnet.public[count.index].id
  route_table_id = aws_route_table.public[0].id
}

resource "aws_eip" "nat" {
  count  = local.private_nat ? 1 : 0
  domain = "vpc"

  tags = { Name = "${local.name}-nat" }
}

resource "aws_nat_gateway" "this" {
  count         = local.private_nat ? 1 : 0
  allocation_id = aws_eip.nat[0].id
  subnet_id     = aws_subnet.public[0].id

  tags = { Name = local.name }

  depends_on = [aws_internet_gateway.this]
}

resource "aws_subnet" "private" {
  count             = local.private_nat ? 2 : 0
  vpc_id            = aws_vpc.this[0].id
  cidr_block        = cidrsubnet(local.vpc_cidr, 8, 10 + count.index)
  availability_zone = data.aws_availability_zones.available[0].names[count.index]

  tags = { Name = "${local.name}-private-${count.index}" }
}

resource "aws_route_table" "private" {
  count  = local.private_nat ? 1 : 0
  vpc_id = aws_vpc.this[0].id

  tags = { Name = "${local.name}-private" }
}

resource "aws_route" "private_nat" {
  count                  = local.private_nat ? 1 : 0
  route_table_id         = aws_route_table.private[0].id
  destination_cidr_block = "0.0.0.0/0"
  nat_gateway_id         = aws_nat_gateway.this[0].id
}

resource "aws_route_table_association" "private" {
  count          = local.private_nat ? 2 : 0
  subnet_id      = aws_subnet.private[count.index].id
  route_table_id = aws_route_table.private[0].id
}
