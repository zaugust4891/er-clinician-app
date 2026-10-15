//! S6 database privileges: what each runtime login may do, derived from its queries.
//!
//! `er-migrate` runs as the schema owner and, after migrating, converges these
//! grants in one transaction: it revokes every table and sequence privilege from
//! `PUBLIC` and all three group roles, then grants exactly [`GRANTS`]. The one-shot
//! database bootstrap creates three NOLOGIN group roles and makes each runtime
//! login a member of one, so rotating a credential never touches a table grant.
//! A relation missing from `GRANTS` gets no runtime access, and a test fails
//! until it is classified. Public-schema routines have no runtime EXECUTE grants.
use sqlx::PgPool;

pub const API_ROLE: &str = "er_api_runtime";
pub const WORKER_ROLE: &str = "er_worker_runtime";
pub const ONBOARDING_ROLE: &str = "er_identity_onboarding";

/// One relation's runtime privileges. An empty string grants nothing.
pub struct Grant {
    pub object: &'static str,
    pub api: &'static str,
    pub worker: &'static str,
    pub onboarding: &'static str,
}

pub const GRANTS: &[Grant] = &[
    // Clinical history is append-only: neither process may UPDATE, DELETE or TRUNCATE.
    Grant {
        object: "TABLE events",
        api: "SELECT, INSERT",
        worker: "SELECT, INSERT",
        onboarding: "",
    },
    Grant {
        object: "SEQUENCE events_sequence_id_seq",
        api: "USAGE",
        worker: "USAGE",
        onboarding: "",
    },
    // The API's snapshot upsert never rewrites its key; the worker only replays.
    Grant {
        object: "TABLE snapshots",
        api: "SELECT, INSERT, UPDATE (org_id, sequence_id, snapshot, checksum, created_at)",
        worker: "SELECT",
        onboarding: "",
    },
    // Membership belongs to onboarding; the API only resolves a token subject.
    Grant {
        object: "TABLE orgs",
        api: "",
        worker: "",
        onboarding: "SELECT, INSERT, UPDATE (name)",
    },
    Grant {
        object: "TABLE clinicians",
        api: "SELECT (id, org_id, cognito_sub)",
        worker: "",
        onboarding: "SELECT, INSERT, UPDATE (org_id, cognito_sub, display_name)",
    },
    Grant {
        object: "TABLE patients",
        api: "SELECT, INSERT",
        worker: "",
        onboarding: "",
    },
    Grant {
        object: "TABLE shifts",
        api: "SELECT, INSERT, UPDATE (ended_at, status)",
        worker: "",
        onboarding: "",
    },
    Grant {
        object: "TABLE shift_patients",
        api: "SELECT, INSERT",
        worker: "",
        onboarding: "",
    },
    // The API mints and completes chunks; the worker transcribes them.
    Grant {
        object: "TABLE audio_chunks",
        api: "SELECT, INSERT, UPDATE (status)",
        worker: "SELECT, UPDATE (status, transcript, duration_ms, error)",
        onboarding: "",
    },
    Grant {
        object: "TABLE jobs",
        api: "SELECT, INSERT, UPDATE (enqueued_at, updated_at)",
        worker: "SELECT, UPDATE (status, error, updated_at)",
        onboarding: "",
    },
    Grant {
        object: "TABLE usage_log",
        api: "",
        worker: "INSERT",
        onboarding: "",
    },
    // Migration bookkeeping is the owner's alone.
    Grant {
        object: "TABLE _sqlx_migrations",
        api: "",
        worker: "",
        onboarding: "",
    },
];

/// Converge the runtime grants as the schema owner, in one transaction.
///
/// Returns `false` only when no group role exists and `required` is false:
/// a development database the bootstrap never ran on. Cloud passes `required`.
pub async fn converge(pool: &PgPool, required: bool) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let roles: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_roles WHERE rolname::text = ANY($1)")
            .bind(vec![API_ROLE, WORKER_ROLE, ONBOARDING_ROLE])
            .fetch_one(&mut *tx)
            .await?;
    match roles {
        0 if !required => return Ok(false),
        3 => {}
        _ => anyhow::bail!("runtime database roles are missing"),
    }
    // GRANT and REVOKE on another role's object can only warn. Refuse rather
    // than leave stale access on a relation or routine we cannot administer.
    let foreign: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (
             SELECT c.relowner AS owner
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'S', 'v', 'm', 'f')
             UNION ALL
             SELECT p.proowner AS owner
               FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
              WHERE n.nspname = 'public'
         ) objects WHERE NOT pg_has_role(current_user, objects.owner, 'USAGE')",
    )
    .fetch_one(&mut *tx)
    .await?;
    anyhow::ensure!(foreign == 0, "schema objects are not owned by this login");

    let from = format!("PUBLIC, {API_ROLE}, {WORKER_ROLE}, {ONBOARDING_ROLE}");
    let statements = vec![
        format!("ALTER DEFAULT PRIVILEGES REVOKE ALL ON TABLES FROM {from}"),
        format!("ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON TABLES FROM {from}"),
        format!("ALTER DEFAULT PRIVILEGES REVOKE ALL ON SEQUENCES FROM {from}"),
        format!("ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE ALL ON SEQUENCES FROM {from}"),
        // Clear global and per-schema defaults for the role creating future
        // objects: per-schema grants are additive to the global default.
        format!("ALTER DEFAULT PRIVILEGES REVOKE EXECUTE ON FUNCTIONS FROM {from}"),
        format!(
            "ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE EXECUTE ON FUNCTIONS FROM {from}"
        ),
        format!("REVOKE ALL ON ALL ROUTINES IN SCHEMA public FROM {from}"),
        format!("REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {from}"),
        format!("REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM {from}"),
    ];
    for statement in &statements {
        sqlx::query(statement).execute(&mut *tx).await?;
    }
    // Table-level REVOKE does not clear existing column ACLs. Remove these too,
    // then reapply the allowlist so a previous broad column grant cannot persist.
    let column_revokes: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT format('REVOKE ALL (%I) ON TABLE %I.%I FROM {from}',
                       a.attname, n.nspname, c.relname)
           FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'public' AND a.attnum > 0
            AND NOT a.attisdropped AND a.attacl IS NOT NULL"
    ))
    .fetch_all(&mut *tx)
    .await?;
    for statement in column_revokes {
        sqlx::query(&statement).execute(&mut *tx).await?;
    }
    for grant in GRANTS {
        for (role, privileges) in [
            (API_ROLE, grant.api),
            (WORKER_ROLE, grant.worker),
            (ONBOARDING_ROLE, grant.onboarding),
        ] {
            if !privileges.is_empty() {
                let object = grant.object;
                sqlx::query(&format!("GRANT {privileges} ON {object} TO {role}"))
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(true)
}

/// Refuse a cloud login more powerful than its process needs: a superuser, a
/// login that can create roles or databases, a replication or `BYPASSRLS` login,
/// or an `rds_superuser` member. The API and worker must also neither own nor be
/// members of the owner of the public schema or any relation or routine in it.
/// Schema ownership can DROP SCHEMA CASCADE even without owning its tables;
/// routine ownership can replace a SECURITY DEFINER body.
pub async fn check_login(pool: &PgPool, process: &str) -> anyhow::Result<()> {
    let overprivileged: bool = sqlx::query_scalar(
        "SELECT r.rolsuper OR r.rolcreaterole OR r.rolcreatedb OR r.rolreplication
                OR r.rolbypassrls
                OR EXISTS (SELECT 1 FROM pg_roles s WHERE s.rolname = 'rds_superuser'
                             AND pg_has_role(r.oid, s.oid, 'MEMBER'))
                OR ($1 AND (
                    has_schema_privilege(r.oid, 'public', 'CREATE')
                    OR has_database_privilege(r.oid, current_database(), 'CREATE')
                    OR EXISTS (
                        SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                         WHERE n.nspname = 'public'
                           AND pg_has_role(r.oid, c.relowner, 'MEMBER'))
                    OR EXISTS (
                        SELECT 1 FROM pg_namespace n
                         WHERE n.nspname = 'public'
                           AND pg_has_role(r.oid, n.nspowner, 'MEMBER'))
                    OR EXISTS (
                        SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                         WHERE n.nspname = 'public'
                           AND pg_has_role(r.oid, p.proowner, 'MEMBER'))))
           FROM pg_roles r WHERE r.rolname = current_user",
    )
    .bind(process != "migration")
    .fetch_one(pool)
    .await
    .map_err(|_| anyhow::anyhow!("database_login_check_failed"))?;
    anyhow::ensure!(!overprivileged, "database_login_overprivileged");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::resolver::ClinicianResolver;
    use crate::repository::{InsertChunkOutcome, NewChunk, NewUsage, PostgresRepository};
    use er_ai::transport::Usage;
    use er_domain::{CaptureMode, NoteEvent, replay::replay};
    use er_store::repository::Repository;
    use sqlx::{Executor, postgres::PgPoolOptions};
    use uuid::Uuid;

    /// Neither runtime role may rewrite history, run DDL, change membership or
    /// read migration bookkeeping.
    const DENIED_TO_BOTH: &[&str] = &[
        "UPDATE events SET event_type = event_type",
        "DELETE FROM events",
        "TRUNCATE events",
        "ALTER TABLE events ADD COLUMN tampered TEXT",
        "DROP TABLE snapshots",
        "DROP SCHEMA public CASCADE",
        "CREATE TABLE tampered (id INT)",
        "INSERT INTO orgs DEFAULT VALUES",
        "UPDATE clinicians SET org_id = org_id",
        "DELETE FROM clinicians",
        "UPDATE snapshots SET patient_id = patient_id",
        "DELETE FROM snapshots",
        "UPDATE audio_chunks SET s3_key = s3_key",
        "DELETE FROM audio_chunks",
        "UPDATE jobs SET kind = kind",
        "DELETE FROM jobs",
        "SELECT version FROM _sqlx_migrations",
    ];
    const DENIED_TO_API: &[&str] = &[
        "SELECT display_name FROM clinicians",
        "INSERT INTO usage_log DEFAULT VALUES",
        "SELECT id FROM usage_log",
    ];
    const DENIED_TO_WORKER: &[&str] = &[
        "SELECT id FROM clinicians",
        "INSERT INTO patients DEFAULT VALUES",
        "SELECT id FROM shifts",
        "INSERT INTO audio_chunks DEFAULT VALUES",
        "INSERT INTO jobs DEFAULT VALUES",
        "UPDATE snapshots SET checksum = checksum",
    ];

    /// The group roles are cluster-wide and shared by parallel test databases.
    /// One statement ensures all three, even after an earlier partial bootstrap.
    async fn ensure_roles(pool: &PgPool) {
        let sql = format!(
            "DO $$ BEGIN
                 BEGIN CREATE ROLE {API_ROLE} NOLOGIN;
                 EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END;
                 BEGIN CREATE ROLE {WORKER_ROLE} NOLOGIN;
                 EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END;
                 BEGIN CREATE ROLE {ONBOARDING_ROLE} NOLOGIN;
                 EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END;
             END $$"
        );
        sqlx::query(&sql).execute(pool).await.unwrap();
    }

    /// Sessions that run as `role`: privileges are checked exactly as for a login
    /// that inherits it, with no password in the test cluster.
    async fn pool_as(pool: &PgPool, role: &'static str) -> PgPool {
        PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |conn, _| {
                Box::pin(async move {
                    conn.execute(format!("SET ROLE {role}").as_str()).await?;
                    Ok(())
                })
            })
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap()
    }

    async fn assert_denied(pool: &PgPool, role: &str, sql: &str) {
        let error = sqlx::query(sql).execute(pool).await.expect_err(sql);
        let code = error.as_database_error().and_then(|db| db.code());
        assert_eq!(code.as_deref(), Some("42501"), "{role}: {sql}");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn every_relation_has_a_privilege_decision(pool: PgPool) {
        let mut found: Vec<String> = sqlx::query_scalar(
            "SELECT CASE c.relkind WHEN 'S' THEN 'SEQUENCE ' ELSE 'TABLE ' END || c.relname::text
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'S', 'v', 'm', 'f')",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let mut decided: Vec<&str> = GRANTS.iter().map(|grant| grant.object).collect();
        found.sort();
        decided.sort();
        assert_eq!(found, decided, "classify every new relation in GRANTS");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn runtime_roles_run_their_code_paths_and_nothing_else(pool: PgPool) {
        ensure_roles(&pool).await;
        assert!(converge(&pool, true).await.unwrap());
        // Every release converges again; a second run must succeed unchanged.
        assert!(converge(&pool, true).await.unwrap());

        // Membership is onboarding's privileged work, outside both runtime roles.
        let (org, clinician) = (Uuid::now_v7(), Uuid::now_v7());
        sqlx::query("INSERT INTO orgs (id, name) VALUES ($1, 'synthetic org')")
            .bind(org)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO clinicians (id, org_id, cognito_sub, display_name)
             VALUES ($1, $2, 'synthetic-sub', 'Synthetic')",
        )
        .bind(clinician)
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();

        // API: resolve a token subject, open a shift, register, append, snapshot,
        // mint and complete audio, receipt an MDM job and end the shift. Retries
        // take each conflict path as well.
        let api = pool_as(&pool, API_ROLE).await;
        let authed = ClinicianResolver::new(api.clone())
            .resolve("synthetic-sub")
            .await
            .unwrap();
        assert_eq!((authed.clinician_id, authed.org_id), (clinician, org));
        let repo = PostgresRepository::for_org(api.clone(), org);
        let shift = repo.create_shift(clinician).await.unwrap().unwrap();
        let (patient, encounter) = (Uuid::now_v7(), Uuid::now_v7());
        for _ in 0..2 {
            repo.register_patient(patient, clinician, "synthetic", None)
                .await
                .unwrap();
        }
        assert!(repo.patient_registered(patient).await.unwrap());
        assert!(repo.patient_row(patient).await.unwrap().is_some());
        assert_eq!(repo.shift_roster(shift.id).await.unwrap().len(), 1);
        let started = NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient);
        let event_id = Uuid::now_v7();
        let sequence = repo
            .append_event(patient, event_id, started.clone())
            .await
            .unwrap();
        let retried = repo
            .append_event(patient, event_id, started.clone())
            .await
            .unwrap();
        assert_eq!(retried, sequence);
        let note = replay(patient, &[started]);
        for _ in 0..2 {
            repo.save_snapshot(patient, sequence, &note).await.unwrap();
        }
        assert!(repo.get_latest_snapshot(patient).await.unwrap().is_some());
        assert_eq!(repo.get_events_since(patient, 0).await.unwrap().len(), 1);
        assert_eq!(repo.event_count().await.unwrap(), 1);
        assert_eq!(repo.snapshot_count().await.unwrap(), 1);
        let chunk = |id, chunk_index, is_final| NewChunk {
            id,
            patient_id: patient,
            encounter_id: encounter,
            chunk_index,
            is_final,
            content_type: "audio/wav",
            s3_key: "synthetic/chunk.wav",
        };
        let (first, last) = (Uuid::now_v7(), Uuid::now_v7());
        let created = repo.insert_chunk(chunk(first, 0, false)).await.unwrap();
        assert!(matches!(created, InsertChunkOutcome::Created(_)));
        let retry = repo.insert_chunk(chunk(first, 0, false)).await.unwrap();
        assert!(matches!(retry, InsertChunkOutcome::Retry(_)));
        repo.insert_chunk(chunk(last, 1, true)).await.unwrap();
        for id in [first, last] {
            repo.mark_chunk_uploaded(id).await.unwrap().unwrap();
        }
        let job = Uuid::now_v7();
        let receipt = repo.insert_job(job, patient, clinician, "mdm").await;
        assert!(receipt.unwrap().created);
        let replayed = repo.insert_job(job, patient, clinician, "mdm").await;
        assert!(!replayed.unwrap().created);
        repo.mark_job_enqueued(job).await.unwrap();
        repo.end_shift(shift.id).await.unwrap().unwrap();

        // Worker: discover the tenant, transcribe and fail chunks, replay the note
        // under the summary lock, append, run the MDM job and meter it.
        let worker = pool_as(&pool, WORKER_ROLE).await;
        let row = PostgresRepository::chunk_row_any_org(&worker, first)
            .await
            .unwrap()
            .unwrap();
        let repo = PostgresRepository::for_org(worker.clone(), row.org_id);
        repo.mark_chunk_transcribing(first).await.unwrap();
        repo.store_chunk_transcript(first, "synthetic", 1_000)
            .await
            .unwrap();
        repo.mark_chunk_transcribing(last).await.unwrap();
        repo.mark_chunk_failed(last, "synthetic_failure")
            .await
            .unwrap();
        assert_eq!(repo.encounter_chunks(encounter).await.unwrap().len(), 2);
        let mut lock = worker.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind("summary:synthetic")
            .execute(&mut *lock)
            .await
            .unwrap();
        er_app::load_patient_note(&repo, patient)
            .await
            .unwrap()
            .unwrap();
        let stopped = NoteEvent::audio_stopped(patient, encounter);
        repo.append_event(patient, Uuid::now_v7(), stopped)
            .await
            .unwrap();
        lock.commit().await.unwrap();
        let discovered = PostgresRepository::job_row_any_org(&worker, job)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(discovered.org_id, org);
        repo.job_row(job).await.unwrap().unwrap();
        repo.mark_job_running(job).await.unwrap().unwrap();
        repo.mark_job_done(job).await.unwrap();
        repo.mark_job_failed(job, "synthetic").await.unwrap();
        let usage = Usage::default();
        let entry = NewUsage {
            clinician_id: Some(clinician),
            patient_id: Some(patient),
            job_id: Some(job),
            kind: "mdm",
            model: "sonnet",
            usage: &usage,
        };
        repo.record_usage(entry).await.unwrap();

        for (role, runtime, denied) in [
            (API_ROLE, &api, DENIED_TO_API),
            (WORKER_ROLE, &worker, DENIED_TO_WORKER),
        ] {
            for sql in DENIED_TO_BOTH.iter().chain(denied) {
                assert_denied(runtime, role, sql).await;
            }
        }
        api.close().await;
        worker.close().await;
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn converge_clears_existing_and_default_routine_access(pool: PgPool) {
        ensure_roles(&pool).await;
        for sql in [
            format!("GRANT UPDATE (event_type) ON events TO {API_ROLE}"),
            format!("GRANT SELECT (display_name) ON clinicians TO {WORKER_ROLE}"),
            format!("ALTER DEFAULT PRIVILEGES GRANT ALL ON TABLES TO {API_ROLE}, {WORKER_ROLE}"),
            format!(
                "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO {API_ROLE}, {WORKER_ROLE}"
            ),
            format!(
                "ALTER DEFAULT PRIVILEGES GRANT EXECUTE ON FUNCTIONS TO {API_ROLE}, {WORKER_ROLE}"
            ),
            format!(
                "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT EXECUTE ON FUNCTIONS
                 TO PUBLIC, {API_ROLE}, {WORKER_ROLE}, {ONBOARDING_ROLE}"
            ),
            "CREATE FUNCTION public.privilege_probe() RETURNS int
             LANGUAGE sql AS 'SELECT 1'"
                .to_owned(),
        ] {
            sqlx::query(&sql).execute(&pool).await.unwrap();
        }
        let api = pool_as(&pool, API_ROLE).await;
        let worker = pool_as(&pool, WORKER_ROLE).await;
        for runtime in [&api, &worker] {
            let value: i32 = sqlx::query_scalar("SELECT public.privilege_probe()")
                .fetch_one(runtime)
                .await
                .unwrap();
            assert_eq!(value, 1);
        }

        assert!(converge(&pool, true).await.unwrap());
        sqlx::query(
            "CREATE FUNCTION public.future_privilege_probe() RETURNS int
             LANGUAGE sql AS 'SELECT 1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        for statement in [
            "CREATE TABLE public.future_acl_probe (id bigint)",
            "CREATE SEQUENCE public.future_acl_probe_seq",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        for (role, runtime) in [(API_ROLE, &api), (WORKER_ROLE, &worker)] {
            assert_denied(runtime, role, "SELECT public.privilege_probe()").await;
            assert_denied(runtime, role, "SELECT public.future_privilege_probe()").await;
            assert_denied(runtime, role, "SELECT * FROM public.future_acl_probe").await;
            assert_denied(
                runtime,
                role,
                "SELECT nextval('public.future_acl_probe_seq')",
            )
            .await;
        }
        assert_denied(&api, API_ROLE, "UPDATE events SET event_type = event_type").await;
        assert_denied(&worker, WORKER_ROLE, "SELECT display_name FROM clinicians").await;
        api.close().await;
        worker.close().await;
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn converge_refuses_a_foreign_routine(pool: PgPool) {
        ensure_roles(&pool).await;
        // A non-superuser owns every relation, but not this routine. The
        // ownership preflight must reject it before REVOKE can merely warn.
        for grant in GRANTS {
            if grant.object.starts_with("TABLE ") {
                let sql = format!("ALTER {} OWNER TO {API_ROLE}", grant.object);
                sqlx::query(&sql).execute(&pool).await.unwrap();
            }
        }
        sqlx::query(
            "CREATE FUNCTION public.foreign_privilege_probe() RETURNS int
             LANGUAGE sql AS 'SELECT 1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let owner = pool_as(&pool, API_ROLE).await;
        let error = converge(&owner, true).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "schema objects are not owned by this login"
        );
        owner.close().await;
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn cloud_login_check_refuses_superusers_and_schema_owners(pool: PgPool) {
        ensure_roles(&pool).await;
        // The test login is a superuser. An RDS master credential is refused too,
        // as a member of `rds_superuser`.
        assert!(check_login(&pool, "migration").await.is_err());
        let api = pool_as(&pool, API_ROLE).await;
        check_login(&api, "api").await.unwrap();

        // Each ownership case is isolated: no table ownership may mask the
        // schema or routine checks. Changes stay within this test database;
        // never alter the shared group roles' attributes or memberships.
        for sql in [
            format!("ALTER TABLE public.usage_log OWNER TO {API_ROLE}"),
            format!("ALTER SCHEMA public OWNER TO {API_ROLE}"),
        ] {
            sqlx::query(&sql).execute(&pool).await.unwrap();
            for process in ["api", "worker"] {
                assert!(check_login(&api, process).await.is_err());
            }
            check_login(&api, "migration").await.unwrap();
            sqlx::query("ALTER TABLE public.usage_log OWNER TO CURRENT_USER")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("ALTER SCHEMA public OWNER TO pg_database_owner")
                .execute(&pool)
                .await
                .unwrap();
            check_login(&api, "api").await.unwrap();
        }

        sqlx::query(
            "CREATE FUNCTION public.owned_privilege_probe() RETURNS int
             LANGUAGE sql AS 'SELECT 1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let transfer = format!("ALTER FUNCTION public.owned_privilege_probe() OWNER TO {API_ROLE}");
        sqlx::query(&transfer).execute(&pool).await.unwrap();
        for process in ["api", "worker"] {
            assert!(check_login(&api, process).await.is_err());
        }
        check_login(&api, "migration").await.unwrap();
        api.close().await;
    }
}
