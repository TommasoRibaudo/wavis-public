# ---------- EC2 Instance ----------
# Previously console-managed. Imported into Terraform via:
#   terraform import aws_instance.wavis i-0123456789abcdef0
#
# After import, run `terraform plan` to verify no destructive changes.
# Terraform manages config but lifecycle rules prevent accidental destruction.

resource "aws_instance" "wavis" {
  ami                    = var.ami_id
  instance_type          = var.instance_type
  subnet_id              = var.subnet_id
  key_name               = var.key_pair_name
  iam_instance_profile   = aws_iam_instance_profile.ec2.name
  vpc_security_group_ids = [aws_security_group.wavis.id]

  root_block_device {
    volume_size           = 30
    volume_type           = "gp3"
    delete_on_termination = true
    encrypted             = false
  }

  metadata_options {
    http_tokens   = "required" # IMDSv2 only
    http_endpoint = "enabled"
  }

  tags = merge(local.tags, {
    Name = "${local.project}-backend-${local.env}"
  })

  lifecycle {
    prevent_destroy = true

    # Don't replace the instance if AMI or user_data changes —
    # we handle OS updates in-place via SSH/SSM.
    ignore_changes = [
      ami,
      user_data,
      user_data_base64,
    ]
  }
}
