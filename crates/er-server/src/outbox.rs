//! Bounded at-least-once recovery for accepted receipts whose queue send failed.
use crate::{
    queue::{self, Job},
    worker::WorkerCtx,
};
use uuid::Uuid;

pub async fn dispatch_once(ctx: &WorkerCtx) -> anyhow::Result<usize> {
    sqlx::query("SELECT public.expire_pending_chunks()")
        .execute(&ctx.pool)
        .await?;
    let rows: Vec<(String, Uuid, Uuid, Uuid)> =
        sqlx::query_as("SELECT kind, id, patient_id, lease FROM public.claim_dispatch()")
            .fetch_all(&ctx.pool)
            .await?;
    let mut sent = 0;
    for (kind, id, patient_id, lease) in rows {
        let job = match kind.as_str() {
            "audio" => Job::TranscribeChunk { chunk_id: id },
            "mdm" => Job::GenerateMdm {
                patient_id,
                job_id: id,
            },
            _ => anyhow::bail!("outbox_kind_invalid"),
        };
        queue::send_job(&ctx.sqs, &ctx.queue_url, &job).await?;
        // If this fails/crashes, lease expiry retries the send; consumers are
        // idempotent. Never acknowledge before SQS accepts the message.
        sqlx::query("SELECT public.ack_dispatch($1)")
            .bind(lease)
            .execute(&ctx.pool)
            .await?;
        sent += 1;
    }
    Ok(sent)
}

pub async fn run(ctx: &WorkerCtx) {
    loop {
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(30), dispatch_once(ctx)).await;
        if !matches!(result, Ok(Ok(_)))
            && crate::telemetry::admit("outbox_dispatch_failed").is_some()
        {
            tracing::warn!(
                error_kind = "outbox_dispatch_failed",
                "dispatch recovery will retry"
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    }
}
