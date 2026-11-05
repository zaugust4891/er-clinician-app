-- Runtime roles are non-owners without BYPASSRLS (checked at cloud startup).
-- The migration owner retains access for backup, onboarding and narrowly scoped
-- SECURITY DEFINER routing functions. Never use its credential for a service.
DO $$
DECLARE relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY['events', 'snapshots', 'patients', 'shifts',
                                    'audio_chunks', 'jobs', 'usage_log'] LOOP
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', relation);
        EXECUTE format('CREATE POLICY tenant ON public.%I USING
            (org_id = nullif(current_setting(''app.org_id'', true), '''')::uuid)
            WITH CHECK (org_id = nullif(current_setting(''app.org_id'', true), '''')::uuid)', relation);
    END LOOP;
END $$;

ALTER TABLE shift_patients ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON shift_patients USING (
    EXISTS (SELECT 1 FROM public.shifts s WHERE s.id = shift_id)
    AND EXISTS (SELECT 1 FROM public.patients p WHERE p.id = patient_id)
) WITH CHECK (
    EXISTS (SELECT 1 FROM public.shifts s WHERE s.id = shift_id)
    AND EXISTS (SELECT 1 FROM public.patients p WHERE p.id = patient_id)
);

CREATE FUNCTION worker_chunk_org(uuid) RETURNS uuid
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS 'SELECT org_id FROM public.audio_chunks WHERE id = $1';
CREATE FUNCTION worker_job_org(uuid) RETURNS uuid
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS 'SELECT org_id FROM public.jobs WHERE id = $1';
REVOKE ALL ON FUNCTION worker_chunk_org(uuid), worker_job_org(uuid) FROM PUBLIC;

ALTER TABLE clinicians ADD COLUMN disabled_at timestamptz;

-- One reusable row per clinician and operation, shared across API replicas.
CREATE TABLE request_quotas (
    org_id uuid NOT NULL REFERENCES orgs(id),
    clinician_id uuid NOT NULL REFERENCES clinicians(id),
    bucket text NOT NULL CHECK (bucket IN ('general', 'mdm', 'audio')),
    window_start timestamptz NOT NULL,
    used integer NOT NULL CHECK (used > 0),
    PRIMARY KEY (org_id, clinician_id, bucket)
);
ALTER TABLE request_quotas ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON request_quotas USING (
    org_id = nullif(current_setting('app.org_id', true), '')::uuid
) WITH CHECK (org_id = nullif(current_setting('app.org_id', true), '')::uuid);

-- Restricted audit evidence, not application logs. No request body or token.
CREATE TABLE access_audit (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES orgs(id),
    clinician_id uuid NOT NULL REFERENCES clinicians(id),
    resource_id uuid,
    route text NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX access_audit_org_time ON access_audit(org_id, recorded_at);
ALTER TABLE access_audit ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON access_audit USING (
    org_id = nullif(current_setting('app.org_id', true), '')::uuid
) WITH CHECK (org_id = nullif(current_setting('app.org_id', true), '')::uuid);

-- Immutable completion receipt. Version IDs select the completed bytes even if
-- a presigned PUT is replayed. ETag conditional reads cover unversioned dev S3.
CREATE TABLE audio_bindings (
    chunk_id uuid PRIMARY KEY REFERENCES audio_chunks(id),
    org_id uuid NOT NULL REFERENCES orgs(id),
    etag text NOT NULL CHECK (length(etag) > 0),
    version_id text
);
ALTER TABLE audio_bindings ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON audio_bindings USING (
    org_id = nullif(current_setting('app.org_id', true), '')::uuid
) WITH CHECK (org_id = nullif(current_setting('app.org_id', true), '')::uuid);

CREATE TABLE dispatch_outbox (
    kind text NOT NULL CHECK (kind IN ('audio', 'mdm')),
    id uuid NOT NULL,
    org_id uuid NOT NULL REFERENCES orgs(id),
    patient_id uuid NOT NULL,
    available_at timestamptz NOT NULL DEFAULT (now() + interval '1 minute'),
    lease uuid,
    PRIMARY KEY (kind, id)
);
CREATE INDEX dispatch_outbox_due ON dispatch_outbox(available_at);
ALTER TABLE dispatch_outbox ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON dispatch_outbox USING (
    org_id = nullif(current_setting('app.org_id', true), '')::uuid
) WITH CHECK (org_id = nullif(current_setting('app.org_id', true), '')::uuid);

-- Triggers make acceptance and dispatch intent indivisible, including a process
-- crash before its first SQS call. Runtime callers cannot insert arbitrary work.
CREATE FUNCTION capture_dispatch() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    IF TG_TABLE_NAME = 'jobs' THEN
        INSERT INTO public.dispatch_outbox(kind,id,org_id,patient_id)
            VALUES ('mdm',NEW.id,NEW.org_id,NEW.patient_id) ON CONFLICT DO NOTHING;
    ELSIF NEW.status = 'uploaded' AND OLD.status = 'pending' THEN
        INSERT INTO public.dispatch_outbox(kind,id,org_id,patient_id)
            VALUES ('audio',NEW.id,NEW.org_id,NEW.patient_id) ON CONFLICT DO NOTHING;
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION capture_dispatch() FROM PUBLIC;
CREATE TRIGGER job_dispatch AFTER INSERT ON jobs FOR EACH ROW EXECUTE FUNCTION capture_dispatch();
CREATE TRIGGER audio_dispatch AFTER UPDATE OF status ON audio_chunks FOR EACH ROW EXECUTE FUNCTION capture_dispatch();

-- Privileged worker operations expose IDs only, claim a bounded batch and
-- cannot be used to read clinical content or select arbitrary SQL/relations.
CREATE FUNCTION claim_dispatch() RETURNS TABLE(kind text, id uuid, patient_id uuid, lease uuid)
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
    WITH due AS (
        SELECT o.kind, o.id FROM public.dispatch_outbox o
        WHERE o.available_at <= now() ORDER BY o.available_at
        LIMIT 10 FOR UPDATE SKIP LOCKED
    )
    UPDATE public.dispatch_outbox o SET available_at = now() + interval '1 minute', lease = gen_random_uuid()
    FROM due WHERE o.kind = due.kind AND o.id = due.id
    RETURNING o.kind, o.id, o.patient_id, o.lease;
$$;
CREATE FUNCTION ack_dispatch(uuid) RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
    DELETE FROM public.dispatch_outbox WHERE lease = $1;
$$;
REVOKE ALL ON FUNCTION claim_dispatch(), ack_dispatch(uuid) FROM PUBLIC;

-- Backfill accepted receipts that may predate the outbox; do not reset jobs
-- already dispatched, running, terminal or awaiting operator DLQ recovery.
INSERT INTO dispatch_outbox(kind,id,org_id,patient_id)
SELECT 'mdm',id,org_id,patient_id FROM jobs WHERE status = 'queued' AND enqueued_at IS NULL;
INSERT INTO dispatch_outbox(kind,id,org_id,patient_id)
SELECT 'audio',id,org_id,patient_id FROM audio_chunks WHERE status = 'uploaded';

-- Expired intents cannot be completed days later with unrelated bytes. Retain
-- the ledger; S3 lifecycle owns object deletion. Never invent a final chunk.
CREATE FUNCTION expire_pending_chunks() RETURNS bigint
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
    WITH due AS (
        SELECT id FROM public.audio_chunks WHERE status = 'pending'
        AND created_at < now() - interval '24 hours'
        ORDER BY created_at LIMIT 1000 FOR UPDATE SKIP LOCKED
    ), expired AS (
        UPDATE public.audio_chunks c SET status = 'failed', error = 'upload_expired'
        FROM due WHERE c.id = due.id RETURNING c.id
    ) SELECT count(*) FROM expired;
$$;
REVOKE ALL ON FUNCTION expire_pending_chunks() FROM PUBLIC;
