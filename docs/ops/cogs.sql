-- Restricted operator reporting, never API credentials. No clinical text output.
-- psql -v org_id=UUID -v from_utc=TIMESTAMP -v to_utc=TIMESTAMP -f docs/ops/cogs.sql
-- Half-open window. For an isolated mini-run, use a dedicated org or exact window.
BEGIN READ ONLY;
SELECT set_config('app.org_id', :'org_id', true);
WITH attributed AS (
 SELECT u.*, coalesce(u.clinician_id,p.created_by) AS attributed_clinician
 FROM usage_log u LEFT JOIN patients p ON p.id=u.patient_id AND p.org_id=u.org_id
 WHERE u.org_id=:'org_id'::uuid AND u.created_at>=:'from_utc'::timestamptz
 AND u.created_at<:'to_utc'::timestamptz
)
SELECT attributed_clinician, kind, model, count(*) AS calls,
 count(DISTINCT patient_id) AS patients,
 sum(input_tokens) AS input_tokens, sum(output_tokens) AS output_tokens,
 sum(cache_read_tokens) AS cache_read_tokens, sum(cache_write_tokens) AS cache_write_tokens,
 sum(stt_seconds) AS stt_seconds,
 sum(cost_micros) AS cost_micros, sum(cost_micros)::numeric/1000000 AS provider_usd,
 sum(cost_micros)::numeric/1000000/nullif(count(DISTINCT patient_id),0) AS provider_usd_per_patient,
 count(*) FILTER (WHERE model LIKE '%unpriced%' OR model LIKE '%mock%') AS invalid_for_invoice_comparison
FROM attributed GROUP BY attributed_clinician,kind,model ORDER BY attributed_clinician,kind,model;
-- Summary/STT attribution falls back to patient creator, not necessarily the
-- recording clinician in a shared-care encounter. Keep unattributed rows visible.
COMMIT;
