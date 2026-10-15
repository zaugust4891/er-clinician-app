//! S6 one-shot database bootstrap, run with the RDS master credential inside the VPC.
//!
//! Creates or updates schema-owner, API, worker and onboarding logins from their
//! staged secret values, makes each runtime login a member of its NOLOGIN group
//! role and lets only the owner create in public. It checks permissions before
//! committing credentials, then reconnects as each login to check authentication.
//! Passwords are sent only as client-computed SCRAM-SHA-256 verifiers, as with
//! psql's password command. er-migrate then creates tables and converges grants
//! as the owner; the first bootstrap must precede the first migration.
//!
//! Authentication checks require a commit. A connection failure or timeout after
//! commit can therefore leave the staged passwords applied; retain those staged
//! values and retry before promoting secrets or starting replacement tasks.
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use crate::privileges::{self, API_ROLE, ONBOARDING_ROLE, WORKER_ROLE};

/// PostgreSQL's default scram_iterations.
const SCRAM_ITERATIONS: u32 = 4096;
const LOGIN_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// A login's name and staged password. Never derive Debug.
pub struct Login {
    pub username: String,
    pub password: String,
}

/// The four logins one bootstrap applies together.
pub struct Logins {
    pub owner: Login,
    pub api: Login,
    pub worker: Login,
    pub onboarding: Login,
}

impl Logins {
    /// Each login with the process privileges::check_login vets it as.
    fn each(&self) -> [(&'static str, &Login); 4] {
        [
            ("migration", &self.owner),
            ("api", &self.api),
            ("worker", &self.worker),
            ("onboarding", &self.onboarding),
        ]
    }

    /// Names must need no quoting and avoid reserved and group roles. Passwords
    /// must look generated: printable ASCII is also what SASLprep leaves
    /// unchanged, so the verifier here equals the one the server would compute.
    pub fn validate(&self) -> anyhow::Result<()> {
        let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_';
        for (process, login) in self.each() {
            let name = login.username.as_str();
            let named = (1..=63).contains(&name.len())
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && name.bytes().all(allowed)
                && !name.starts_with("pg_")
                && !name.starts_with("rds")
                && ![API_ROLE, WORKER_ROLE, ONBOARDING_ROLE].contains(&name);
            anyhow::ensure!(named, "{process} login name is invalid");
            let password = login.password.as_bytes();
            let generated =
                (32..=128).contains(&password.len()) && password.iter().all(u8::is_ascii_graphic);
            anyhow::ensure!(
                generated,
                "{process} password must be 32 to 128 printable ASCII characters"
            );
        }
        let names = self.each().map(|(_, login)| login.username.as_str());
        anyhow::ensure!(
            names
                .iter()
                .enumerate()
                .all(|(i, name)| !names[..i].contains(name)),
            "login names must be distinct"
        );
        Ok(())
    }
}

/// Replace a database error with the step that failed: the caller prints this
/// text, and a server message must never carry a statement.
fn failed(step: &'static str) -> impl FnOnce(sqlx::Error) -> anyhow::Error {
    move |_| anyhow::anyhow!("{step}_failed")
}

/// Apply staged credentials in one transaction, then prove each login.
pub async fn apply(pool: &PgPool, logins: &Logins) -> anyhow::Result<()> {
    logins.validate()?;
    let mut tx = pool.begin().await.map_err(failed("begin"))?;
    sqlx::query("SET LOCAL search_path = pg_catalog, public")
        .execute(&mut *tx)
        .await
        .map_err(failed("search_path"))?;
    let bootstrap: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut *tx)
        .await
        .map_err(failed("identity"))?;
    let reused = logins.each().iter().any(|(_, l)| l.username == bootstrap);
    anyhow::ensure!(!reused, "a login may not reuse the bootstrap credential");

    // Group roles hold every table grant, so a rotation never touches one. A
    // concurrent creation is tolerated; their attributes are checked below.
    for group in [API_ROLE, WORKER_ROLE, ONBOARDING_ROLE] {
        let create = format!(
            "DO $$ BEGIN CREATE ROLE {group} NOLOGIN;
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
        );
        sqlx::query(&create)
            .execute(&mut *tx)
            .await
            .map_err(failed("group_role"))?;
    }
    for (process, login) in logins.each() {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname::text = $1)")
                .bind(login.username.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(failed("login_lookup"))?;
        // ALTER keeps attributes it does not name; check_staged_login below
        // refuses elevated attributes before this transaction can commit.
        let template = if exists {
            "ALTER ROLE %I WITH LOGIN INHERIT PASSWORD %L"
        } else {
            "CREATE ROLE %I WITH LOGIN INHERIT PASSWORD %L"
        };
        // A salt needs uniqueness, not secrecy: a v4 UUID has 122 OS-random bits.
        let verifier = scram_verifier(&login.password, Uuid::new_v4().as_bytes());
        let args = [login.username.as_str(), verifier.as_str()];
        execute_formatted(&mut tx, template, &args)
            .await
            .map_err(|_| anyhow::anyhow!("{process}_login_failed"))?;
    }
    let memberships = [
        (API_ROLE, logins.api.username.as_str()),
        (WORKER_ROLE, logins.worker.username.as_str()),
        (ONBOARDING_ROLE, logins.onboarding.username.as_str()),
    ];
    for (group, login) in memberships {
        execute_formatted(&mut tx, "GRANT %I TO %I", &[group, login])
            .await
            .map_err(failed("membership"))?;
    }
    // Only the owner creates objects. Grant runtime USAGE explicitly rather
    // than depending on PUBLIC retaining it in a previously hardened database.
    let owner = logins.owner.username.as_str();
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut *tx)
        .await
        .map_err(failed("database"))?;
    execute_formatted(
        &mut tx,
        "REVOKE ALL ON DATABASE %I FROM PUBLIC",
        &[&database],
    )
    .await
    .map_err(failed("database_revoke"))?;
    for role in [owner, API_ROLE, WORKER_ROLE, ONBOARDING_ROLE] {
        execute_formatted(
            &mut tx,
            "GRANT CONNECT ON DATABASE %I TO %I",
            &[&database, role],
        )
        .await
        .map_err(failed("database_connect"))?;
    }
    sqlx::query("REVOKE CREATE ON SCHEMA public FROM PUBLIC")
        .execute(&mut *tx)
        .await
        .map_err(failed("schema_revoke"))?;
    execute_formatted(
        &mut tx,
        "GRANT USAGE, CREATE ON SCHEMA public TO %I",
        &[owner],
    )
    .await
    .map_err(failed("schema_grant"))?;
    let runtime_usage =
        format!("GRANT USAGE ON SCHEMA public TO {API_ROLE}, {WORKER_ROLE}, {ONBOARDING_ROLE}");
    sqlx::query(&runtime_usage)
        .execute(&mut *tx)
        .await
        .map_err(failed("schema_usage"))?;

    // An existing PostgreSQL 16 membership retains unspecified options on
    // GRANT. Reject ADMIN rights and non-inheriting memberships too.
    let members = vec![
        owner,
        logins.api.username.as_str(),
        logins.worker.username.as_str(),
        logins.onboarding.username.as_str(),
        ONBOARDING_ROLE,
        API_ROLE,
        WORKER_ROLE,
    ];
    let mut found: Vec<(String, String, bool, bool)> = sqlx::query_as(
        "SELECT m.rolname::text, g.rolname::text, a.admin_option, a.inherit_option
           FROM pg_auth_members a
           JOIN pg_roles m ON m.oid = a.member
           JOIN pg_roles g ON g.oid = a.roleid
          WHERE m.rolname::text = ANY($1)",
    )
    .bind(members)
    .fetch_all(&mut *tx)
    .await
    .map_err(failed("membership_check"))?;
    found.sort();
    let mut planned = memberships.map(|(g, l)| (l.to_owned(), g.to_owned(), false, true));
    planned.sort();
    anyhow::ensure!(found == planned, "login memberships differ from the plan");
    let plain_groups: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_roles
          WHERE rolname::text = ANY($1)
            AND NOT (rolcanlogin OR rolsuper OR rolcreaterole OR rolcreatedb
                     OR rolreplication OR rolbypassrls)",
    )
    .bind(vec![API_ROLE, WORKER_ROLE, ONBOARDING_ROLE])
    .fetch_one(&mut *tx)
    .await
    .map_err(failed("group_check"))?;
    anyhow::ensure!(plain_groups == 3, "runtime group roles must be NOLOGIN");
    for (process, login) in logins.each() {
        check_staged_login(&mut tx, process, login).await?;
    }
    tx.commit().await.map_err(failed("commit"))?;

    // Prove authentication after commit, over the bootstrap's TLS settings.
    // Failures here do not undo the committed credentials.
    for (process, login) in logins.each() {
        let options = (*pool.connect_options())
            .clone()
            .username(&login.username)
            .password(&login.password);
        let check = async {
            let login_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .map_err(|_| anyhow::anyhow!("{process}_login_connect_failed"))?;
            let checked = privileges::check_login(&login_pool, process).await;
            login_pool.close().await;
            checked.map_err(|error| anyhow::anyhow!("{process}_{error}"))
        };
        tokio::time::timeout(LOGIN_CHECK_TIMEOUT, check)
            .await
            .map_err(|_| anyhow::anyhow!("{process}_login_check_timeout"))??;
    }
    Ok(())
}

/// Inspect staged roles through the privileged transaction before changing any
/// committed password. Runtime grants must come through the group roles:
/// converge() does not revoke direct grants from individual logins.
async fn check_staged_login(
    conn: &mut PgConnection,
    process: &str,
    login: &Login,
) -> anyhow::Result<()> {
    let unsafe_role: bool = sqlx::query_scalar(
        "SELECT NOT r.rolcanlogin OR r.rolsuper OR r.rolcreaterole OR r.rolcreatedb
                OR r.rolreplication OR r.rolbypassrls
                OR EXISTS (SELECT 1 FROM pg_roles s WHERE s.rolname = 'rds_superuser'
                             AND pg_has_role(r.oid, s.oid, 'MEMBER'))
                OR ($2 AND (
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
                           AND pg_has_role(r.oid, p.proowner, 'MEMBER'))
                    OR EXISTS (
                        SELECT 1 FROM (
                            SELECT c.relacl AS acl
                              FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                             WHERE n.nspname = 'public'
                            UNION ALL
                            SELECT a.attacl
                              FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid
                              JOIN pg_namespace n ON n.oid = c.relnamespace
                             WHERE n.nspname = 'public'
                            UNION ALL
                            SELECT p.proacl
                              FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                             WHERE n.nspname = 'public'
                            UNION ALL
                            SELECT d.defaclacl FROM pg_default_acl d
                             WHERE d.defaclnamespace = 0
                                OR d.defaclnamespace = 'public'::regnamespace
                        ) grants CROSS JOIN LATERAL aclexplode(grants.acl) a
                        WHERE a.grantee = r.oid)))
           FROM pg_roles r WHERE r.rolname::text = $1",
    )
    .bind(&login.username)
    .bind(process != "migration")
    .fetch_one(conn)
    .await
    .map_err(failed("staged_login_check"))?;
    anyhow::ensure!(!unsafe_role, "{process}_login_overprivileged");
    Ok(())
}

/// Quote with the server's own format(), never by hand. Do not cache a prepared
/// statement carrying a verifier on the connection.
async fn execute_formatted(
    conn: &mut PgConnection,
    template: &str,
    args: &[&str],
) -> Result<(), sqlx::Error> {
    let statement: String = sqlx::query_scalar("SELECT format($1, VARIADIC $2::text[])")
        .bind(template)
        .bind(args.to_vec())
        .fetch_one(&mut *conn)
        .await?;
    sqlx::query(&statement)
        .persistent(false)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// HMAC-SHA-256, the pseudorandom function of SCRAM-SHA-256.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// The SCRAM-SHA-256$<iterations>:<salt>$<StoredKey>:<ServerKey> form PostgreSQL
/// stores and accepts in place of a password (RFC 5802 and RFC 7677).
fn scram_verifier(password: &str, salt: &[u8]) -> String {
    // Hi() is PBKDF2-HMAC-SHA-256 producing one 32-byte block.
    let password = password.as_bytes();
    let mut message = salt.to_vec();
    message.extend_from_slice(&1u32.to_be_bytes());
    let mut block = hmac_sha256(password, &message);
    let mut salted = block;
    for _ in 1..SCRAM_ITERATIONS {
        block = hmac_sha256(password, &block);
        for (byte, next) in salted.iter_mut().zip(block) {
            *byte ^= next;
        }
    }
    let stored_key = Sha256::digest(hmac_sha256(&salted, b"Client Key"));
    let server_key = hmac_sha256(&salted, b"Server Key");
    format!(
        "SCRAM-SHA-256${SCRAM_ITERATIONS}:{}${}:{}",
        STANDARD.encode(salt),
        STANDARD.encode(stored_key),
        STANDARD.encode(server_key)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "synthetic-Password-0123456789-abcdef";
    const ROTATED: &str = "rotated-Password-0123456789-abcdefgh";

    fn logins(suffix: &str, password: &str) -> Logins {
        let login = |role: &str| Login {
            username: format!("er_{role}_{suffix}"),
            password: password.to_owned(),
        };
        Logins {
            owner: login("owner"),
            api: login("api"),
            worker: login("worker"),
            onboarding: login("onboarding"),
        }
    }

    async fn connect_as(pool: &PgPool, login: &Login) -> PgPool {
        let options = (*pool.connect_options())
            .clone()
            .username(&login.username)
            .password(&login.password);
        PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap()
    }

    async fn stored_verifier(pool: &PgPool, name: &str) -> String {
        sqlx::query_scalar("SELECT rolpassword FROM pg_authid WHERE rolname::text = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// PostgreSQL computes its own verifier for a known password; ours, given
    /// the same salt, must match it exactly.
    #[sqlx::test(migrations = false)]
    async fn verifier_matches_the_servers_own(pool: PgPool) {
        let name = format!("er_scram_probe_{}", Uuid::now_v7().simple());
        let mut conn = pool.acquire().await.unwrap();
        for sql in [
            "SET password_encryption = 'scram-sha-256'",
            "SET scram_iterations = 4096",
        ] {
            sqlx::query(sql).execute(&mut *conn).await.unwrap();
        }
        let args = [name.as_str(), PASSWORD];
        execute_formatted(&mut conn, "CREATE ROLE %I PASSWORD %L", &args)
            .await
            .unwrap();
        let stored = stored_verifier(&pool, &name).await;
        let drop_role = format!("DROP ROLE {name}");
        sqlx::query(&drop_role).execute(&mut *conn).await.unwrap();
        let salt = stored
            .strip_prefix("SCRAM-SHA-256$4096:")
            .and_then(|rest| rest.split_once('$'))
            .map(|(salt, _)| STANDARD.decode(salt).unwrap())
            .unwrap();
        assert_eq!(scram_verifier(PASSWORD, &salt), stored);
    }

    #[sqlx::test(migrations = false)]
    async fn bootstrap_creates_rotates_and_limits_every_login(pool: PgPool) {
        let suffix = Uuid::now_v7().simple().to_string();
        let first = logins(&suffix, PASSWORD);
        apply(&pool, &first).await.unwrap();
        let mut before = Vec::new();
        for (_, login) in first.each() {
            before.push(stored_verifier(&pool, &login.username).await);
        }

        // A rerun is the rotation path: the same logins with new staged passwords.
        let rotated = logins(&suffix, ROTATED);
        apply(&pool, &rotated).await.unwrap();
        for (_, login) in first.each() {
            let options = (*pool.connect_options())
                .clone()
                .username(&login.username)
                .password(&login.password);
            assert!(
                PgPoolOptions::new()
                    .max_connections(1)
                    .connect_with(options)
                    .await
                    .is_err(),
                "the previous password must no longer authenticate"
            );
        }
        for ((_, login), old) in rotated.each().into_iter().zip(&before) {
            let new = stored_verifier(&pool, &login.username).await;
            assert!(new.starts_with("SCRAM-SHA-256$4096:"));
            assert_ne!(&new, old);
        }

        // The owner migrates and converges grants; neither runtime login may then
        // run DDL or delete history.
        let owner = connect_as(&pool, &rotated.owner).await;
        sqlx::migrate!("./migrations").run(&owner).await.unwrap();
        assert!(privileges::converge(&owner, true).await.unwrap());
        owner.close().await;
        let onboarding = connect_as(&pool, &rotated.onboarding).await;
        let org = Uuid::now_v7();
        sqlx::query("INSERT INTO orgs (id, name) VALUES ($1, 'synthetic org')")
            .bind(org)
            .execute(&onboarding)
            .await
            .unwrap();
        sqlx::query("INSERT INTO clinicians (id, org_id, cognito_sub, display_name) VALUES ($1, $2, $3, 'Synthetic')")
            .bind(Uuid::now_v7()).bind(org).bind(format!("synthetic-{suffix}"))
            .execute(&onboarding).await.unwrap();
        for sql in [
            "SELECT * FROM events",
            "DELETE FROM orgs",
            "CREATE TABLE tampered (id INT)",
        ] {
            let error = sqlx::query(sql).execute(&onboarding).await.expect_err(sql);
            assert_eq!(
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .as_deref(),
                Some("42501")
            );
        }
        onboarding.close().await;
        for login in [&rotated.api, &rotated.worker] {
            let runtime = connect_as(&pool, login).await;
            for sql in [
                "CREATE TABLE tampered (id INT)",
                "DROP TABLE events",
                "DELETE FROM events",
            ] {
                let error = sqlx::query(sql).execute(&runtime).await.expect_err(sql);
                let code = error.as_database_error().and_then(|db| db.code());
                assert_eq!(code.as_deref(), Some("42501"), "{sql}");
            }
            runtime.close().await;
        }

        // Drift on a pre-existing login must fail before committing any new
        // password. Mutate only this test's login and its memberships.
        let api = &rotated.api.username;
        let drift = [
            (
                format!("ALTER ROLE {api} CREATEDB"),
                format!("ALTER ROLE {api} NOCREATEDB"),
            ),
            (
                format!("GRANT {API_ROLE} TO {api} WITH ADMIN TRUE"),
                format!("REVOKE ADMIN OPTION FOR {API_ROLE} FROM {api}"),
            ),
            (
                format!("GRANT {API_ROLE} TO {api} WITH INHERIT FALSE"),
                format!("GRANT {API_ROLE} TO {api} WITH INHERIT TRUE"),
            ),
            (
                format!("GRANT CREATE ON SCHEMA public TO {api}"),
                format!("REVOKE CREATE ON SCHEMA public FROM {api}"),
            ),
            (
                format!("GRANT DELETE ON events TO {api}"),
                format!("REVOKE DELETE ON events FROM {api}"),
            ),
            (
                format!("GRANT UPDATE (org_id) ON clinicians TO {api}"),
                format!("REVOKE UPDATE (org_id) ON clinicians FROM {api}"),
            ),
        ];
        for (prepare, undo) in drift {
            sqlx::query(&prepare).execute(&pool).await.unwrap();
            let mut committed = Vec::new();
            for (_, login) in rotated.each() {
                committed.push(stored_verifier(&pool, &login.username).await);
            }
            let error = apply(&pool, &first).await.unwrap_err();
            assert!(!format!("{error:#}").contains("synthetic-"));
            sqlx::query(&undo).execute(&pool).await.unwrap();
            for ((_, login), expected) in rotated.each().into_iter().zip(committed) {
                assert_eq!(stored_verifier(&pool, &login.username).await, expected);
            }
        }
        // The restored plan must remain repeatable after every rejected attempt.
        apply(&pool, &rotated).await.unwrap();

        // Unsafe staged values fail before any change and are never echoed.
        let current: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut reused = logins(&suffix, PASSWORD);
        reused.owner.username = current;
        let mut duplicate = logins(&suffix, PASSWORD);
        duplicate.worker.username = duplicate.api.username.clone();
        let mut group = logins(&suffix, PASSWORD);
        group.api.username = API_ROLE.to_owned();
        let mut spaced = logins(&suffix, PASSWORD);
        spaced.worker.password = format!("{PASSWORD} with spaces");
        for case in [
            reused,
            duplicate,
            group,
            spaced,
            logins(&suffix, "synthetic-short"),
        ] {
            let error = apply(&pool, &case).await.unwrap_err();
            assert!(!format!("{error:#}").contains("synthetic-"));
        }

        // Roles are cluster-wide: remove this test's logins and what they own.
        let roles = rotated
            .each()
            .map(|(_, login)| login.username.as_str())
            .join(", ");
        for sql in [
            format!("DROP OWNED BY {roles}"),
            format!("DROP ROLE {roles}"),
        ] {
            sqlx::query(&sql).execute(&pool).await.unwrap();
        }
    }
}
