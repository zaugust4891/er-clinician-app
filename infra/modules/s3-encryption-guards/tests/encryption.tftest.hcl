variables {
  bucket_arn  = "arn:aws:s3:::synthetic-policy-test"
  kms_key_arn = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"
}
run "encryption_request_cases" {
  command = plan
  assert {
    condition = length(output.statements) == 4 && alltrue([
      for statement in output.statements : statement.Effect == "Deny" &&
      statement.Principal == "*" && statement.Action == "s3:PutObject" &&
      statement.Resource == "${var.bucket_arn}/*"
    ])
    error_message = "Encryption guards must be scoped to object writes in the requested bucket."
  }
  # Evaluate only the encryption denies over independent request cases. This
  # exercises the IAM condition logic; real S3/key grants remain live checks.
  assert {
    condition = alltrue([
      for request in [
        { name = "headerless", context = {}, denied = false },
        { name = "approved KMS", context = { "s3:x-amz-server-side-encryption" = "aws:kms", "s3:x-amz-server-side-encryption-aws-kms-key-id" = var.kms_key_arn }, denied = false },
        { name = "SSE-S3", context = { "s3:x-amz-server-side-encryption" = "AES256" }, denied = true },
        { name = "wrong KMS key", context = { "s3:x-amz-server-side-encryption" = "aws:kms", "s3:x-amz-server-side-encryption-aws-kms-key-id" = "arn:aws:kms:us-east-2:000000000000:key/wrong-key" }, denied = true },
        { name = "KMS with no key", context = { "s3:x-amz-server-side-encryption" = "aws:kms" }, denied = true },
        { name = "SSE-C", context = { "s3:x-amz-server-side-encryption-customer-algorithm" = "AES256" }, denied = true }
        ] : anytrue([
          for statement in output.statements : alltrue([
            for operator, entries in statement.Condition : alltrue([
              for key, expected in entries :
              operator == "Null" ? tostring(!contains(keys(request.context), key)) == expected :
              operator == "StringEquals" ? lookup(request.context, key, "") == expected :
              operator == "StringNotEquals" ? lookup(request.context, key, "") != expected : false
            ])
          ]) if contains(["DenyWrongEncryptionAlgorithm", "DenyWrongEncryptionKey", "DenyKmsWithoutKey", "DenyCustomerProvidedEncryption"], statement.Sid)
      ]) == request.denied
    ])
    error_message = "Encryption guards must allow headerless/approved-key requests and deny SSE-S3, wrong keys, KMS without a key and SSE-C."
  }
}
