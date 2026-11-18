CREATE TABLE device_capture_audit (
    org_id UUID NOT NULL REFERENCES orgs(id),
    patient_id UUID NOT NULL REFERENCES patients(id),
    clinician_id UUID NOT NULL REFERENCES clinicians(id),
    version BIGINT NOT NULL CHECK(version > 0),
    payload JSONB NOT NULL,
    PRIMARY KEY(org_id,patient_id,clinician_id)
);
ALTER TABLE device_capture_audit ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON device_capture_audit USING (
    org_id = NULLIF(current_setting('app.org_id',true),'')::uuid
) WITH CHECK (
    org_id = NULLIF(current_setting('app.org_id',true),'')::uuid
);
