CREATE TABLE usage_log (
    id                  UUID PRIMARY KEY,
    org_id              UUID NOT NULL REFERENCES orgs(id),
    clinician_id        UUID REFERENCES clinicians(id),
    patient_id          UUID,
    job_id              UUID,
    kind                TEXT NOT NULL CHECK (kind IN ('summarize', 'mdm', 'stt')),
    model               TEXT,
    input_tokens        BIGINT,
    output_tokens       BIGINT,
    cache_read_tokens   BIGINT,
    cache_write_tokens  BIGINT,
    stt_seconds         INT,
    cost_micros         BIGINT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_usage_org_time ON usage_log (org_id, created_at DESC);

CREATE TABLE jobs (
    id              UUID PRIMARY KEY,
    org_id          UUID NOT NULL REFERENCES orgs(id),
    clinician_id    UUID NOT NULL REFERENCES clinicians(id),
    patient_id      UUID NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('mdm')),
    status          TEXT NOT NULL DEFAULT 'queued'
                    CHECK (status IN ('queued', 'running', 'done', 'failed')),
    error           TEXT,
    enqueued_at     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_jobs_status ON jobs (status, created_at);
CREATE INDEX idx_jobs_org_patient ON jobs (org_id, patient_id, created_at DESC);
