# Test helper: evaluate an ECR lifecycle policy against a synthetic image history
# the way ECR documents it. Rules apply in priority order; an image belongs to the
# first rule whose tag selection matches it and no later rule can expire it.
# `tagged` with prefixes needs every prefix to match some tag. Count rules rank
# each rule's images by push time, newest first, which is the order of `images`.
terraform {
  required_version = ">= 1.10.0, < 2.0.0"
}

variable "policy" {
  type = string
}

variable "images" {
  description = "Newest push first."
  type = list(object({
    name     = string
    tags     = list(string)
    age_days = number
  }))
}

locals {
  rules = [for rule in jsondecode(var.policy).rules : rule]

  # matches[image][rule]: whether that rule's selection covers that image.
  matches = [for image in var.images : [for rule in local.rules : rule.selection.tagStatus == "any" || (rule.selection.tagStatus == "untagged" && length(image.tags) == 0) || (rule.selection.tagStatus == "tagged" && length(image.tags) > 0 && alltrue([for prefix in try(rule.selection.tagPrefixList, []) : anytrue([for tag in image.tags : startswith(tag, prefix)])]))]]

  # The image's governing rule, or -1 when no rule selects it (never expired).
  first_rule = [for row in local.matches : try(index(row, true), -1)]

  # Whether the governing rule's age or count threshold is exceeded.
  over = [for position, rule_index in local.first_rule : try(local.rules[rule_index].selection.countType == "sinceImagePushed" ? var.images[position].age_days > local.rules[rule_index].selection.countNumber : length([for earlier in range(position + 1) : earlier if local.first_rule[earlier] == rule_index]) > local.rules[rule_index].selection.countNumber, false)]

  expired = [for position, image in var.images : image.name if local.first_rule[position] >= 0 && local.over[position]]
}

output "expired" {
  value = local.expired
}
