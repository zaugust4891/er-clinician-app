
CREATE TABLE orgs (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now() 
);

CREATE TABLE clinicians (
    id              UUID PRIMARY KEY,
    org_id          UUID NOT NULL REFERENCES orgs(id),
    cognito_sub     TEXT NOT NULL UNIQUE, -- the JWT 'sub' <- the join key (UNIQUE => indexed)
    display_name    TEXT NOT NULL,
    role            TEXT NOT NULL DEFAULT 'physician',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Back-fill the FKs the events migration couldn't declare (orgs didn't exist yet).
-- Fine in dev where events/snapshots are empty or test data; if orphan org_ids exist,
-- either seed those orgs first or `sqlx database reset` (dev data is disposable).
ALTER TABLE events ADD CONSTRAINT fk_events_org FOREIGN KEY (org_id) REFERENCES orgs(id);
ALTER TABLE snapshots ADD CONSTRAINT fk_snapshots_org FOREIGN KEY (org_id) REFERENCES orgs(id);

-- Postgres indexes the referenced side of an FK (orgs.id, via its PK) but not
-- the referencing side. events.org_id is already covered by idx_events_org;
-- index the other two so org-scoped queries never seq-scan.
CREATE INDEX idx_snapshots_org  ON snapshots (org_id);
CREATE INDEX idx_clinicians_org ON clinicians (org_id);
