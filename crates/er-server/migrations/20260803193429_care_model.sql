CREATE TABLE patients (
    id                  UUID PRIMARY KEY, -- client-stamped UUIDv7 (S3)
    org_id              UUID NOT NULL REFERENCES orgs(id),
    created_by          UUID NOT NULL REFERENCES clinicians(id), -- audit: who registered them
    chief_complaint     TEXT NOT NULL,
    display_label       TEXT, -- "Room 4" — NEVER a name (HIPAA)
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_patients_org ON patients (org_id);

CREATE TABLE shifts (
    id              UUID PRIMARY KEY,
    org_id          UUID NOT NULL REFERENCES orgs(id),
    clinician_id    UUID NOT NULL REFERENCES clinicians(id), -- from the TOKEN, never the body
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at        TIMESTAMPTZ,
    status          TEXT NOT NULL DEFAULT 'active'  -- 'active' | 'ended'
);

CREATE UNIQUE INDEX one_active_shift_per_clinician
    ON shifts (clinician_id) WHERE status = 'active';

CREATE INDEX idx_shifts_org ON shifts (org_id);

CREATE TABLE shift_patients (
    shift_id       UUID NOT NULL REFERENCES shifts(id),
    patient_id     UUID NOT NULL REFERENCES patients(id),
    position       INT NOT NULL, -- ShiftManager's stable list order, now durable
    PRIMARY KEY (shift_id, patient_id), -- composite key: a patient appears once per shift
    UNIQUE (shift_id, position) -- and each slot is used once (see note below)
);