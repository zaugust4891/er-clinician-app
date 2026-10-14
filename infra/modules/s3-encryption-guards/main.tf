# Pure policy statements shared by audio and bootstrap. No AWS resources are created.
# Headerless uploads use the bucket default. Explicit SSE-KMS must name its approved
# key; otherwise S3 selects aws/s3. SSE-S3, DSSE-KMS and SSE-C overrides are denied.
variable "bucket_arn" {
  type = string
}
variable "kms_key_arn" {
  type = string
}
output "statements" {
  value = [
    {
      Sid       = "DenyWrongEncryptionAlgorithm"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:PutObject"
      Resource  = "${var.bucket_arn}/*"
      Condition = {
        Null = {
          "s3:x-amz-server-side-encryption" = "false"
        }
        StringNotEquals = {
          "s3:x-amz-server-side-encryption" = "aws:kms"
        }
      }
    },
    {
      Sid       = "DenyWrongEncryptionKey"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:PutObject"
      Resource  = "${var.bucket_arn}/*"
      Condition = {
        Null = {
          "s3:x-amz-server-side-encryption-aws-kms-key-id" = "false"
        }
        StringNotEquals = {
          "s3:x-amz-server-side-encryption-aws-kms-key-id" = var.kms_key_arn
        }
      }
    },
    # An explicit aws:kms header without a key selects AWS's aws/s3 key rather
    # than this bucket's customer-managed default (general-purpose S3 buckets).
    {
      Sid       = "DenyKmsWithoutKey"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:PutObject"
      Resource  = "${var.bucket_arn}/*"
      Condition = {
        StringEquals = { "s3:x-amz-server-side-encryption" = "aws:kms" }
        Null         = { "s3:x-amz-server-side-encryption-aws-kms-key-id" = "true" }
      }
    },
    {
      Sid       = "DenyCustomerProvidedEncryption"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:PutObject"
      Resource  = "${var.bucket_arn}/*"
      Condition = { Null = { "s3:x-amz-server-side-encryption-customer-algorithm" = "false" } }
    }
  ]
}
