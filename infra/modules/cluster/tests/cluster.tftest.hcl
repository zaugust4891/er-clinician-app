# Mocked plans: no AWS credentials and nothing created. Synthetic identifiers only.
mock_provider "aws" {
}

variables {
  name = "ernote-synthetic"
}

run "repository_is_immutable_scanned_and_never_expires_tagged_images" {
  command = apply

  assert {
    condition = (
      aws_ecr_repository.app.name == "ernote-synthetic" &&
      aws_ecr_repository.app.image_tag_mutability == "IMMUTABLE" &&
      aws_ecr_repository.app.image_scanning_configuration[0].scan_on_push &&
      !aws_ecr_repository.app.force_delete
    )
    error_message = "The repository must use immutable tags, scan on push and refuse forced deletion."
  }

  # No rule selects a tagged image, so there is no count or age threshold at which
  # a running, rollback or failed-promotion image could be expired.
  assert {
    condition = (
      length(jsondecode(output.lifecycle_policy).rules) == 1 &&
      jsondecode(output.lifecycle_policy).rules[0].selection.tagStatus == "untagged" &&
      jsondecode(output.lifecycle_policy).rules[0].selection.countType == "sinceImagePushed" &&
      jsondecode(output.lifecycle_policy).rules[0].selection.countNumber == 7
    )
    error_message = "Only untagged pushes may expire, by age; no rule may select tagged images."
  }

  assert {
    condition = (
      aws_ecs_cluster.this.name == "ernote-synthetic" &&
      aws_ecs_cluster_capacity_providers.this.capacity_providers == toset(["FARGATE"]) &&
      one(aws_ecs_cluster.this.setting).value == "disabled"
    )
    error_message = "The cluster is Fargate-only, and Container Insights waits for S9."
  }
}

# 200 failed promotions pushed after the accepted image, an accepted image and a
# rollback target pushed long ago, and never-released candidates of any age: none
# expire. Only the stale untagged push does.
run "no_tagged_image_expires_regardless_of_count_or_age" {
  command = apply

  module {
    source = "./tests/simulate"
  }

  variables {
    policy = run.repository_is_immutable_scanned_and_never_expires_tagged_images.lifecycle_policy
    images = concat(
      [for i in range(200) : { name = "failed-${i}", tags = ["sha-f${i}", "release-f${i}"], age_days = i }],
      [
        { name = "untagged-fresh", tags = [], age_days = 1 },
        { name = "candidate-fresh", tags = ["sha-c1"], age_days = 3 },
        { name = "untagged-stale", tags = [], age_days = 10 },
        { name = "current-accepted", tags = ["sha-a", "release-a"], age_days = 4000 },
        { name = "candidate-stale", tags = ["sha-c2"], age_days = 4500 },
        { name = "rollback-target", tags = ["sha-r", "release-r"], age_days = 40000 },
      ]
    )
  }

  assert {
    condition     = output.expired == ["untagged-stale"]
    error_message = "Only stale untagged pushes may expire; got ${jsonencode(output.expired)}."
  }
}

# Control: the simulator does apply count rules, so the run above is meaningful.
# Any count rule over released images, however large, has such a threshold.
run "a_count_rule_over_released_images_evicts_the_running_image" {
  command = apply

  module {
    source = "./tests/simulate"
  }

  variables {
    policy = jsonencode({ rules = [{ rulePriority = 1, selection = { tagStatus = "tagged", tagPrefixList = ["release-"], countType = "imageCountMoreThan", countNumber = 20 }, action = { type = "expire" } }] })
    images = concat(
      [for i in range(30) : { name = "failed-${i}", tags = ["release-f${i}"], age_days = i }],
      [
        { name = "current-accepted", tags = ["release-a"], age_days = 40 },
        { name = "rollback-target", tags = ["release-r"], age_days = 400 },
      ]
    )
  }

  assert {
    condition     = contains(output.expired, "current-accepted") && contains(output.expired, "rollback-target")
    error_message = "The simulator must reproduce count-based eviction."
  }
}
