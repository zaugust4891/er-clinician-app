locals {
  # The environment is a committed constant, not an input. A root cannot be pointed at
  # the wrong environment by passing a different variable, and the release root checks
  # that the manifest it reads carries the environment it expects.
  environment  = "staging"
  manifest_key = "envs/staging/foundation-manifest.json"

  # S3 bucket names are global, so this one carries the account and region to stay
  # unique — and nothing else: no clinician, patient, or hospital identifier. It is
  # composed here rather than inside the data module because the network module needs
  # the same string for its S3 gateway endpoint policy. Passing one computed name into
  # both modules keeps the dependency one-way (network -> data) instead of circular.
  audio_bucket_name = "${var.project}-${local.environment}-audio-${var.aws_account_id}-${var.aws_region}"
  audio_bucket_arn  = "arn:aws:s3:::${local.audio_bucket_name}"

  # ---------------------------------------------------------------------------
  # The foundation manifest's payload
  # ---------------------------------------------------------------------------
  # Step 8 S3 establishes this contract; S4-S9 fill it in as each component lands.
  # Keys expected by the end of Step 8, in the order they arrive:
  #
  #   S4  vpc_id, private_subnet_ids, api_security_group_id, worker_security_group_id
  #   S5  audio_bucket, stt_queue_name, stt_queue_url, stt_dlq_url, db_endpoint,
  #       db_port, db_name, data_kms_key_arn
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
  manifest_resources = merge(local.edge_manifest_resources, {
    deploy_artifacts_bucket = var.deploy_artifacts_bucket
    }, {
    vpc_id                       = module.network[0].vpc_id
    private_subnet_ids           = jsonencode(module.network[0].private_subnet_ids)
    api_security_group_id        = module.network[0].security_group_ids.api
    worker_security_group_id     = module.network[0].security_group_ids.worker
    migration_security_group_id  = module.network[0].security_group_ids.migration
    onboarding_security_group_id = module.network[0].security_group_ids.onboarding
    api_https_port               = tostring(module.network[0].api_https_port)

    # S5. `audio_bucket` and `stt_queue_name` are the literal values of the API's and
    # worker's AUDIO_BUCKET and STT_QUEUE; the queue URLs are published for operators
    # and S9 alarms, since the application resolves its own URLs with GetQueueUrl.
    # `db_endpoint` is a hostname with no port, because `db.rs` verifies that hostname
    # against the server certificate and composes the port separately.
    # The database login, its password, and the RDS-managed master secret ARN are
    # deliberately absent: S6 owns those, and no release needs them.
    audio_bucket     = module.data[0].audio_bucket_name
    stt_queue_name   = module.data[0].stt_queue_name
    stt_queue_url    = module.data[0].stt_queue_url
    stt_dlq_url      = module.data[0].stt_dlq_url
    db_endpoint      = module.data[0].db_host
    db_port          = tostring(module.data[0].db_port)
    db_name          = module.data[0].db_name
    data_kms_key_arn = module.data[0].kms_key_arn

    # S6: roles and pointers only. Privileged bootstrap/master references remain
    # available through the operator output, outside the release manifest.
    api_task_role_arn            = module.identity[0].task_role_arns.api
    worker_task_role_arn         = module.identity[0].task_role_arns.worker
    migration_task_role_arn      = module.identity[0].task_role_arns.migration
    api_execution_role_arn       = module.identity[0].execution_role_arns.api
    worker_execution_role_arn    = module.identity[0].execution_role_arns.worker
    migration_execution_role_arn = module.identity[0].execution_role_arns.migration

    # S8: where release task definitions find their image and run.
    ecr_repository_url = module.cluster[0].repository_url
    ecs_cluster_arn    = module.cluster[0].cluster_arn

    # S7 (next map), only once the pool exists: the API's COGNITO_* values, nothing secret.
    }, {
    for key, value in {
      cognito_pool_id   = one(module.auth[*].user_pool_id)
      cognito_client_id = one(module.auth[*].client_id)
      cognito_region    = var.aws_region
    } : key => value if var.auth.enabled
  })

  edge_manifest_resources = local.edge_enabled ? {
    api_target_group_arn        = module.edge[0].api_target_group_arn
    api_base_url                = module.edge[0].api_base_url
    alb_arn                     = module.edge[0].alb_arn
    alb_arn_suffix              = module.edge[0].alb_arn_suffix
    api_target_group_arn_suffix = module.edge[0].target_group_arn_suffix
    waf_acl_arn                 = module.edge[0].waf_acl_arn
  } : {}

  manifest_secret_refs = {
    api_db       = module.identity[0].secret_arns.api_db
    worker_db    = module.identity[0].secret_arns.worker_db
    migration_db = module.identity[0].secret_arns.migration_db
    deepgram     = module.identity[0].secret_arns.deepgram
  }

  # Staging always publishes its manifest.
  manifest_enabled = true
}
