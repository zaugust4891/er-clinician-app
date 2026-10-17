mock_provider "aws" {}
variables {
  family             = "ernote-staging-db-bootstrap"
  component          = "db-bootstrap"
  image              = "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-staging@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  aws_region         = "us-east-2"
  execution_role_arn = "arn:aws:iam::000000000000:role/ernote-staging-db-bootstrap-execution"
  environment        = { APP_ENV = "staging", DB_MAX_CONNECTIONS = "1" }
  secrets            = { DB_PASSWORD = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-AbCdEf:password::" }
  log_group          = "/ecs/ernote-staging/db-bootstrap"
}
run "bootstrap_has_no_task_role_or_listener" {
  command = apply
  assert {
    condition = (
      !contains(keys(output.definition), "taskRoleArn") &&
      output.definition.containerDefinitions[0].entryPoint == ["/usr/local/bin/er-db-bootstrap"] &&
      length(output.definition.containerDefinitions[0].portMappings) == 0 &&
      output.definition.containerDefinitions[0].readonlyRootFilesystem &&
      !contains(keys(output.definition.containerDefinitions[0]), "command") &&
      jsondecode(output.register_payload).executionRoleArn == var.execution_role_arn
    )
    error_message = "Bootstrap is a private one-shot with only an execution role and exact binary, compatible with db-secrets.py."
  }
}
run "onboarding_uses_its_own_binary_and_role" {
  command = apply
  variables {
    family        = "ernote-staging-onboarding"
    component     = "onboarding"
    task_role_arn = "arn:aws:iam::000000000000:role/ernote-staging-onboarding-task"
  }
  assert {
    condition     = output.definition.containerDefinitions[0].entryPoint == ["/usr/local/bin/er-onboard"] && output.definition.taskRoleArn == var.task_role_arn
    error_message = "Onboarding must use the onboarding binary and task role."
  }
}
run "reject_tag_reference" {
  command = plan
  variables { image = "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-staging:latest" }
  expect_failures = [var.image]
}
