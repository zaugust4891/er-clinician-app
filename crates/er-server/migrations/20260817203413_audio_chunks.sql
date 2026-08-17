
CREATE TABLE audio_chunks (
    id              UUID PRIMARY KEY, --client stamped. Retries are re-send.
    org_id           UUID NOT NULL REFERENCES orgs(id), -- tenancy captured at mint time. The worker resumes it.
    patient_id      UUID NOT NULL, -- no FK - same reasoning as events.patient_id
    encounter_id    UUID NOT NULL, -- the API layer enforces registration, not the schema
    chunk_index     INT NOT NULL,
    is_final        BOOLEAN NOT NULL DEFAULT FALSE, -- finality is data on a chunk.
    content_type    TEXT NOT NULL, -- 'audio/wav', 'audio/ogg', audio/mp4' - signed into the URL
    s3_key          TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending', -- pending -> uploaded -> transcribing -> done | failed
    transcript      TEXT, -- per-chunk STT output; assembled in index order
    error           TEXT, -- terminal failure reason, for the autopsy
    duration_ms     INT, -- FROM deepgram metadata.duration;
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One upload slot per position: a retry with the SAME chunk_id lands on the PK and is
-- idempotent (S3 handler); a DIFFERENT chunk_id claiming the same slot is a client bug
-- and must be a loud 409, not a silent second row.
CREATE UNIQUE INDEX idx_chunks_encounter_index ON audio_chunks (encounter_id, chunk_index);

-- Exactly one final chunk per encounter — the same partial-unique-index referee that
-- enforced "one active shift per clinician": the index only contains rows
-- WHERE is_final, so it only ever arbitrates finality claims.
CREATE UNIQUE INDEX idx_chunks_one_final ON audio_chunks (encounter_id) WHERE is_final;

-- Assembly (S7) and the status sweep (Step 9) both walk an encounter's chunks in order.
CREATE INDEX idx_chunks_encounter ON audio_chunks (encounter_id, chunk_index, status);

