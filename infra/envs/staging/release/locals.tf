locals {
  environment  = "staging"
  manifest_key = "envs/staging/foundation-manifest.json"

  # Production reads nothing until it is enabled; staging always reads.
  manifest_required = true

  # Keys this root cannot deploy without. Step 8 S8 adds the ECS services that need
  # them; add each key here at the same time the foundation starts publishing it, so a
  # half-applied foundation fails the release plan instead of producing a broken task
  # definition.
  #
  # Expected by the end of Step 8: private_subnet_ids, api_security_group_id,
  # worker_security_group_id, ecr_repository_url, ecs_cluster_arn, api_target_group_arn,
  # api_task_role_arn, worker_task_role_arn, api_execution_role_arn,
  # worker_execution_role_arn, audio_bucket, stt_queue_name, api_log_group,
  # worker_log_group.
  #
  # The S5 keys below are the ones a task definition turns directly into environment:
  # AUDIO_BUCKET, STT_QUEUE, and the host/port/name half of the database configuration
  # whose credentials are injected from Secrets Manager. The queue URLs and the data key
  # ARN are published for operators and S9 without being required here.
  required_resource_keys = [
    "deploy_artifacts_bucket",
    "vpc_id",
    "private_subnet_ids",
    "api_security_group_id",
    "worker_security_group_id",
    "migration_security_group_id",
    "onboarding_security_group_id",
    "api_https_port",
    "audio_bucket",
    "stt_queue_name",
    "db_endpoint",
    "db_port",
    "db_name",
  ]

  manifest = local.manifest_required ? jsondecode(data.aws_s3_object.manifest[0].body) : null
}
