resource "aws_s3_bucket_public_access_block" "index" {
  count  = var.quickwit_index_s3_prefix == "" ? 1 : 0
  bucket = aws_s3_bucket.index[0].id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_versioning" "index" {
  count  = var.quickwit_index_s3_prefix == "" ? 1 : 0
  bucket = aws_s3_bucket.index[0].id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "index" {
  count  = var.quickwit_index_s3_prefix == "" ? 1 : 0
  bucket = aws_s3_bucket.index[0].id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_policy" "index_deny_insecure_transport" {
  count  = var.quickwit_index_s3_prefix == "" ? 1 : 0
  bucket = aws_s3_bucket.index[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid       = "DenyInsecureTransport"
        Effect    = "Deny"
        Principal = "*"
        Action    = "s3:*"
        Resource = [
          aws_s3_bucket.index[0].arn,
          "${aws_s3_bucket.index[0].arn}/*",
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
