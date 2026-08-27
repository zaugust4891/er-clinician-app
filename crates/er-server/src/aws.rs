//! AWS client construction and the local audio-infrastructure bootstrap.
//!
//! MinIO and ElasticMQ implement the S3 and SQS wire protocols, so the rest of
//! the application uses the official AWS SDK in every environment. Endpoint
//! overrides are the only switch: present locally, absent in ECS.

use anyhow::{Context, anyhow};
use aws_config::{BehaviorVersion, SdkConfig};
use aws_sdk_s3::Client as S3Client;
use aws_sdk_sqs::{Client as SqsClient, types::QueueAttributeName};

const AUDIO_JOB_VISIBILITY_TIMEOUT_SECS: &str = "300";
const AUDIO_JOB_MAX_RECEIVE_COUNT: &str = "3";

#[derive(Clone)]
pub struct AwsClients {
    pub s3: S3Client,
    pub sqs: SqsClient,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueUrls {
    pub stt: String,
    pub stt_dlq: String,
}

/// Load the normal AWS region and credential-provider chain once, then derive
/// service clients from it. In development the explicit endpoints redirect the
/// signed requests to MinIO and ElasticMQ. In ECS, `None` leaves normal AWS
/// endpoint and task-role resolution intact.
pub async fn clients(s3_endpoint: Option<&str>, sqs_endpoint: Option<&str>) -> AwsClients {
    let shared = aws_config::defaults(BehaviorVersion::latest()).load().await;
    AwsClients {
        s3: s3_client(&shared, s3_endpoint),
        sqs: sqs_client(&shared, sqs_endpoint),
    }
}

pub fn s3_client(shared: &SdkConfig, endpoint: Option<&str>) -> S3Client {
    let mut builder = aws_sdk_s3::config::Builder::from(shared);
    if let Some(endpoint) = endpoint {
        // MinIO expects `host/bucket/key`; real S3 normally uses
        // `bucket.host/key` and therefore does not need this override.
        builder = builder.endpoint_url(endpoint).force_path_style(true);
    }
    S3Client::from_conf(builder.build())
}

pub fn sqs_client(shared: &SdkConfig, endpoint: Option<&str>) -> SqsClient {
    let mut builder = aws_sdk_sqs::config::Builder::from(shared);
    if let Some(endpoint) = endpoint {
        builder = builder.endpoint_url(endpoint);
    }
    SqsClient::from_conf(builder.build())
}

/// Construct clients that are safe to place in `AppState::for_tests` without
/// doing network I/O or starting the AWS credential-provider chain. Existing
/// HTTP/repository tests never call them; S1's audio integration tests inject
/// their own clients through `for_tests_with_audio`.
pub fn lazy_test_clients(s3_endpoint: &str, sqs_endpoint: &str) -> AwsClients {
    let s3 = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .credentials_provider(aws_sdk_s3::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "er-server-tests",
        ))
        .endpoint_url(s3_endpoint)
        .force_path_style(true)
        .build();
    let sqs = aws_sdk_sqs::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(aws_sdk_sqs::config::Region::new("us-east-1"))
        .credentials_provider(aws_sdk_sqs::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "er-server-tests",
        ))
        .endpoint_url(sqs_endpoint)
        .build();

    AwsClients {
        s3: S3Client::from_conf(s3),
        sqs: SqsClient::from_conf(sqs),
    }
}

/// Create the local bucket and queue pair if they are absent, attach the DLQ
/// redrive policy, and return queue URLs ready for Send/ReceiveMessage calls.
///
/// This is deliberately called only when endpoint overrides are configured.
/// Terraform owns resource creation in staging and production.
pub async fn ensure_audio_infra(
    s3: &S3Client,
    sqs: &SqsClient,
    bucket: &str,
    queue: &str,
    dlq: &str,
) -> anyhow::Result<QueueUrls> {
    match s3.create_bucket().bucket(bucket).send().await {
        Ok(_) => {}
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service| service.is_bucket_already_owned_by_you()) => {}
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!("failed to create local audio bucket {bucket}")));
        }
    }

    // The main queue's RedrivePolicy embeds the DLQ ARN, so creation order is
    // observable and important: DLQ -> ARN lookup -> main queue.
    let dlq_output = sqs
        .create_queue()
        .queue_name(dlq)
        .send()
        .await
        .with_context(|| format!("failed to create local STT dead-letter queue {dlq}"))?;
    let dlq_url = dlq_output
        .queue_url()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("SQS did not return a URL after creating queue {dlq}"))?;

    let attributes = sqs
        .get_queue_attributes()
        .queue_url(&dlq_url)
        .attribute_names(QueueAttributeName::QueueArn)
        .send()
        .await
        .with_context(|| format!("failed to read ARN for STT dead-letter queue {dlq}"))?;
    let dlq_arn = attributes
        .attributes()
        .and_then(|attrs| attrs.get(&QueueAttributeName::QueueArn))
        .cloned()
        .ok_or_else(|| anyhow!("SQS did not return an ARN for queue {dlq}"))?;

    let main_output = sqs
        .create_queue()
        .queue_name(queue)
        .attributes(
            QueueAttributeName::VisibilityTimeout,
            AUDIO_JOB_VISIBILITY_TIMEOUT_SECS,
        )
        .attributes(QueueAttributeName::RedrivePolicy, redrive_policy(&dlq_arn))
        .send()
        .await
        .with_context(|| format!("failed to create local STT queue {queue}"))?;
    let main_url = main_output
        .queue_url()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("SQS did not return a URL after creating queue {queue}"))?;

    Ok(QueueUrls {
        stt: main_url,
        stt_dlq: dlq_url,
    })
}

/// Resolve Terraform-created production queues without attempting to create or
/// mutate infrastructure from the application process. The Step 8 ECS task
/// roles must therefore include `sqs:GetQueueUrl` for both queue resources.
pub async fn resolve_queue_urls(
    sqs: &SqsClient,
    queue: &str,
    dlq: &str,
) -> anyhow::Result<QueueUrls> {
    let stt = get_queue_url(sqs, queue).await?;
    let stt_dlq = get_queue_url(sqs, dlq).await?;
    Ok(QueueUrls { stt, stt_dlq })
}

async fn get_queue_url(sqs: &SqsClient, queue: &str) -> anyhow::Result<String> {
    sqs.get_queue_url()
        .queue_name(queue)
        .send()
        .await
        .with_context(|| format!("failed to resolve SQS queue {queue}"))?
        .queue_url()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("SQS did not return a URL for queue {queue}"))
}

fn redrive_policy(dlq_arn: &str) -> String {
    serde_json::json!({
        "deadLetterTargetArn": dlq_arn,
        "maxReceiveCount": AUDIO_JOB_MAX_RECEIVE_COUNT,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redrive_policy_points_at_dlq_and_limits_retries() {
        let policy: serde_json::Value =
            serde_json::from_str(&redrive_policy("arn:aws:sqs:us-east-1:123:stt-dlq"))
                .expect("valid redrive JSON");

        assert_eq!(
            policy["deadLetterTargetArn"],
            "arn:aws:sqs:us-east-1:123:stt-dlq"
        );
        assert_eq!(policy["maxReceiveCount"], "3");
    }
}
