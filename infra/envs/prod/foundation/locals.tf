locals {
  # The environment is a committed constant, not an input. A root cannot be pointed at
  # the wrong environment by passing a different variable, and the release root checks
  # that the manifest it reads carries the environment it expects.
  environment  = "prod"
  manifest_key = "envs/prod/foundation-manifest.json"

  # ---------------------------------------------------------------------------
  # The foundation manifest's payload
  # ---------------------------------------------------------------------------
  # Step 8 S3 establishes this contract; S4-S9 fill it in as each component lands.
  # Keys expected by the end of Step 8, in the order they arrive:
  #
  #   S4  vpc_id, private_subnet_ids, api_security_group_id, worker_security_group_id
  #   S5  audio_bucket, stt_queue_url, stt_dlq_url, db_endpoint, db_port, db_name,
  #       data_kms_key_arn
  #   S6  api_task_role_arn, worker_task_role_arn, api_execution_role_arn,
  #       worker_execution_role_arn, migration_task_role_arn,
  #       migration_execution_role_arn
  #   S7  cognito_pool_id, cognito_client_id, cognito_region
  #   S8  ecr_repository_url, ecs_cluster_arn, api_target_group_arn
  #   S9  api_log_group, worker_log_group, migration_log_group, alarm_topic_arn
  #
  # Secrets Manager ARNs belong in manifest_secret_refs. Secret values belong in
  # neither, and the module rejects them.
  # The v1 manifest is map(string): subnet lists are JSON arrays encoded as strings.
  # Release decodes private_subnet_ids with jsondecode, never split on commas.
  manifest_resources = merge({
    deploy_artifacts_bucket = var.deploy_artifacts_bucket
    }, var.enabled ? {
    vpc_id                       = module.network[0].vpc_id
    private_subnet_ids           = jsonencode(module.network[0].private_subnet_ids)
    api_security_group_id        = module.network[0].security_group_ids.api
    worker_security_group_id     = module.network[0].security_group_ids.worker
    migration_security_group_id  = module.network[0].security_group_ids.migration
    onboarding_security_group_id = module.network[0].security_group_ids.onboarding
    api_https_port               = tostring(module.network[0].api_https_port)
  } : {})

  manifest_secret_refs = {}

  # Production publishes nothing until the root is explicitly enabled.
  manifest_enabled = var.enabled
}
