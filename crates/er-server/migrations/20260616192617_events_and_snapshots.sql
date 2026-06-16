-- Add migration script here
CREATE TABLE events (
    sequence_id          BIGSERIAL PRIMARY KEY,         
    event_id             UUID NOT NULL UNIQUE,          
    org_id               UUID NOT NULL,                 
    patient_id           UUID NOT NULL,
    event_type           TEXT NOT NULL,
    payload              JSONB NOT NULL,              
    clinical_occurred_at TIMESTAMPTZ NOT NULL,          
    recorded_at          TIMESTAMPTZ NOT NULL DEFAULT now()  
);
CREATE INDEX idx_events_patient_seq ON events (patient_id, sequence_id);
CREATE INDEX idx_events_org         ON events (org_id, recorded_at);

CREATE TABLE snapshots (
    patient_id  UUID PRIMARY KEY,
    org_id      UUID NOT NULL,
    sequence_id BIGINT NOT NULL,
    snapshot    TEXT NOT NULL,         
    checksum    TEXT NOT NULL,         
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);