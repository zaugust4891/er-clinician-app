mock_provider "aws" {
  mock_data "aws_caller_identity" { defaults = { account_id = "000000000000" } }
  mock_data "aws_iam_policy_document" { defaults = { json = "{}" } }
}
variables { aws_account_id = "000000000000" }
run "no_oidc_trust_by_default" {
  command = plan
  assert {
    condition     = length(aws_iam_openid_connect_provider.github) == 0
    error_message = "Existing bootstrap must not create an OIDC provider implicitly."
  }
}
run "opt_in_provider_has_sts_audience" {
  command = plan
  variables { github_oidc_enabled = true }
  assert {
    condition     = aws_iam_openid_connect_provider.github[0].url == "https://token.actions.githubusercontent.com" && aws_iam_openid_connect_provider.github[0].client_id_list == toset(["sts.amazonaws.com"])
    error_message = "The account-wide GitHub provider must accept only the STS audience."
  }
}
