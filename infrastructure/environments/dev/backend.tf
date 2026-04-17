terraform {
  backend "s3" {
    bucket         = "wavis-terraform-state-1"
    key            = "dev/terraform.tfstate"
    region         = "us-east-2"
    dynamodb_table = "wavis-terraform-locks"
    encrypt        = true
  }
}
