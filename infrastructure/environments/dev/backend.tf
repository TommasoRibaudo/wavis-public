terraform {
  backend "s3" {
    bucket         = "example-terraform-state-bucket"
    key            = "dev/terraform.tfstate"
    region         = "us-east-2"
    dynamodb_table = "wavis-terraform-locks"
    encrypt        = true
  }
}
