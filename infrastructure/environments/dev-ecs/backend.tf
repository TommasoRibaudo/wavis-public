terraform {
  backend "s3" {
    bucket         = "example-terraform-state-bucket"
    key            = "dev-ecs/terraform.tfstate"
    region         = "us-east-2"
    dynamodb_table = "wavis-terraform-locks"
    encrypt        = true
  }
}
