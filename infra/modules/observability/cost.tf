# Account-level billing signal; service and clinician allocation remains the SQL
# receipt report. Billing metrics require account opt-in and are published in us-east-1.
resource "aws_cloudwatch_dashboard" "cost" {
  dashboard_name = "${var.name}-cost"
  dashboard_body = jsonencode({ widgets = [
    { type = "metric", x = 0, y = 0, width = 24, height = 6, properties = {
      title   = "Account estimated charges (USD; delayed, not an invoice)", region = "us-east-1", period = 21600, stat = "Maximum",
      metrics = [["AWS/Billing", "EstimatedCharges", "Currency", "USD"]]
    } },
    { type = "text", x = 0, y = 6, width = 24, height = 3, properties = {
      markdown = "Provider COGS: run docs/ops/cogs.sql for a restricted org/time window. Reconcile Bedrock and Deepgram independently against settled provider exports using scripts/reconcile-costs.py. Activate the existing Project and Env cost allocation tags. Account estimates include shared and unrelated resources; never divide them by clinicians without an allocation decision."
    } }
  ] })
}
