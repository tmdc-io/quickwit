terraform {
  required_version = "1.7.5"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.39.1"
    }
  }
}

provider "aws" {
  region = "us-east-1"
  default_tags {
    tags = {
      provisioner = "terraform"
      author      = "Quickwit"
    }
  }
}

locals {
  sqs_notification_queue_name = "qw-tuto-s3-event-notifications"
  source_bucket_name          = "qw-tuto-source-bucket"
}

resource "aws_s3_bucket" "file_source" {
  bucket_prefix = local.source_bucket_name
  force_destroy = true
}

resource "aws_s3_bucket_public_access_block" "file_source" {
  bucket = aws_s3_bucket.file_source.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_versioning" "file_source" {
  bucket = aws_s3_bucket.file_source.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "file_source" {
  bucket = aws_s3_bucket.file_source.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_policy" "file_source_deny_insecure_transport" {
  bucket = aws_s3_bucket.file_source.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid       = "DenyInsecureTransport"
        Effect    = "Deny"
        Principal = "*"
        Action    = "s3:*"
        Resource = [
          aws_s3_bucket.file_source.arn,
          "${aws_s3_bucket.file_source.arn}/*",
        ]
        Condition = {
          Bool = {
            "aws:SecureTransport" = "false"
          }
        }
      },
    ]
  })
}

data "aws_iam_policy_document" "sqs_notification" {
  statement {
    effect = "Allow"

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    actions   = ["sqs:SendMessage"]
    resources = ["arn:aws:sqs:*:*:${local.sqs_notification_queue_name}"]

    condition {
      test     = "ArnEquals"
      variable = "aws:SourceArn"
      values   = [aws_s3_bucket.file_source.arn]
    }
  }
}


resource "aws_sqs_queue" "s3_events" {
  name   = local.sqs_notification_queue_name
  policy = data.aws_iam_policy_document.sqs_notification.json

  sqs_managed_sse_enabled = true

  redrive_policy = jsonencode({
    deadLetterTargetArn = aws_sqs_queue.s3_events_deadletter.arn
    maxReceiveCount     = 5
  })
}

resource "aws_sqs_queue" "s3_events_deadletter" {
  name                    = "${local.sqs_notification_queue_name}-deadletter"
  sqs_managed_sse_enabled = true
}

resource "aws_sqs_queue_redrive_allow_policy" "s3_events_deadletter" {
  queue_url = aws_sqs_queue.s3_events_deadletter.id

  redrive_allow_policy = jsonencode({
    redrivePermission = "byQueue",
    sourceQueueArns   = [aws_sqs_queue.s3_events.arn]
  })
}

resource "aws_s3_bucket_notification" "bucket_notification" {
  bucket = aws_s3_bucket.file_source.id

  queue {
    queue_arn = aws_sqs_queue.s3_events.arn
    events    = ["s3:ObjectCreated:*"]
  }
}

data "aws_iam_policy_document" "quickwit_node" {
  statement {
    effect = "Allow"
    actions = [
      "sqs:ReceiveMessage",
      "sqs:DeleteMessage",
      "sqs:ChangeMessageVisibility",
      "sqs:GetQueueAttributes",
    ]
    resources = [aws_sqs_queue.s3_events.arn]
  }
  statement {
    effect    = "Allow"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.file_source.arn}/*"]
  }
}

data "aws_caller_identity" "current" {}

resource "aws_iam_policy" "quickwit_node" {
  name   = "quickwit-filesource-tutorial"
  path   = "/system/"
  policy = data.aws_iam_policy_document.quickwit_node.json
}

resource "aws_iam_user" "quickwit_node" {
  name = "quickwit-filesource-tutorial"
  path = "/system/"
}

resource "aws_iam_user_policy_attachment" "quickwit_node" {
  user       = aws_iam_user.quickwit_node.name
  policy_arn = aws_iam_policy.quickwit_node.arn
}

resource "aws_iam_access_key" "quickwit_node" {
  user = aws_iam_user.quickwit_node.name
}

output "source_bucket_name" {
  value = aws_s3_bucket.file_source.bucket

}

output "notification_queue_url" {
  value = aws_sqs_queue.s3_events.id
}

output "quickwit_node_access_key_id" {
  value     = aws_iam_access_key.quickwit_node.id
  sensitive = true
}

output "quickwit_node_secret_access_key" {
  value     = aws_iam_access_key.quickwit_node.secret
  sensitive = true
}
