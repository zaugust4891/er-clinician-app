-- Run as a restricted reporting operator after draining the worker. Inputs as cogs.sql.
BEGIN READ ONLY;
SELECT set_config('app.org_id', :'org_id', true);
SELECT event_id,count(*) FROM events WHERE org_id=:'org_id'::uuid
 GROUP BY event_id HAVING count(*)>1;
SELECT status,count(*) FROM jobs WHERE org_id=:'org_id'::uuid
 AND created_at>=:'from_utc'::timestamptz AND created_at<:'to_utc'::timestamptz GROUP BY status;
SELECT status,count(*) FROM audio_chunks WHERE org_id=:'org_id'::uuid
 AND created_at>=:'from_utc'::timestamptz AND created_at<:'to_utc'::timestamptz GROUP BY status;
SELECT count(*) AS undispatched FROM dispatch_outbox WHERE org_id=:'org_id'::uuid;
COMMIT;
-- Also record SQS main/DLQ visible AND invisible depths using the operator role;
-- k6 cannot prove DLQ emptiness. Terminal failure, leftovers, or missing work fail acceptance.
