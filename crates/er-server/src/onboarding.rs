//! S7 tenant onboarding: the only path that grants clinical access.
//!
//! A Cognito login proves a subject; the `clinicians` row decides its org. This
//! one-shot operation runs inside the VPC as the onboarding login, whose grants
//! cover only `orgs` and `clinicians`. One run applies every entry in a single
//! transaction, so a refused entry leaves nothing half-applied.
//!
//! Re-running the same request is a no-op. A subject already bound to a different
//! org is refused, never moved: pool replacement or a copy-paste error must
//! surface instead of silently transferring a clinician's access. Revocation
//! rebinds the row's subject to the tombstone `revoked:<sub>` rather than
//! deleting it, because patients and shifts reference the clinician id. The
//! tombstone keeps the subject-to-org binding: a revoked subject is never
//! re-granted or moved by a later request; restoring access is a reviewed
//! manual change, not a seeding re-run.
use std::collections::BTreeSet;

use sqlx::PgPool;
use uuid::Uuid;

/// One intended org membership. `cognito_sub` is the user's actual `sub`
/// attribute, fetched from the pool, never a username or email.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    pub org_id: Uuid,
    pub org_name: String,
    pub cognito_sub: String,
    pub display_name: String,
}

/// A subject whose access to `org_id` ends.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revocation {
    pub org_id: Uuid,
    pub cognito_sub: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Created,
    Unchanged,
    /// Same org; only the display name changed.
    Renamed,
    Revoked,
    /// Revocation of a subject that has no row in that org.
    Absent,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Unchanged => "unchanged",
            Self::Renamed => "renamed",
            Self::Revoked => "revoked",
            Self::Absent => "absent",
        }
    }
}

/// Names a staging run may store. Staging holds synthetic identities only.
pub const SYNTHETIC_PREFIX: &str = "Synthetic ";

/// Cognito subjects are lowercase hyphenated UUIDs; anything else is almost
/// certainly a username, email or pasted token, and would never match a JWT.
fn check_sub(sub: &str) -> anyhow::Result<()> {
    let canonical = Uuid::parse_str(sub).is_ok_and(|id| id.hyphenated().to_string() == sub);
    anyhow::ensure!(canonical, "cognito_sub must be a lowercase UUID subject");
    Ok(())
}

fn check_name(field: &str, value: &str, synthetic_only: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1..=80).contains(&value.chars().count())
            && value.trim() == value
            && !value.chars().any(char::is_control),
        "{field} must be 1 to 80 printable characters"
    );
    anyhow::ensure!(
        !synthetic_only || value.starts_with(SYNTHETIC_PREFIX),
        "{field} must start with \"{SYNTHETIC_PREFIX}\" in staging"
    );
    Ok(())
}

/// Validate a whole request before opening a transaction. A subject may appear
/// once: two entries for one subject are either redundant or a transfer.
pub fn validate(memberships: &[Membership], synthetic_only: bool) -> anyhow::Result<()> {
    anyhow::ensure!(!memberships.is_empty(), "no memberships requested");
    let mut subs = BTreeSet::new();
    for m in memberships {
        check_sub(&m.cognito_sub)?;
        check_name("org_name", &m.org_name, synthetic_only)?;
        check_name("display_name", &m.display_name, synthetic_only)?;
        anyhow::ensure!(
            subs.insert(m.cognito_sub.as_str()),
            "a subject may appear only once per request"
        );
    }
    Ok(())
}

/// Replace a database error with the step that failed; server messages can
/// carry values and are never printed.
fn failed(step: &'static str) -> impl FnOnce(sqlx::Error) -> anyhow::Error {
    move |_| anyhow::anyhow!("{step}_failed")
}

/// The subject a revoked row carries; no Cognito `sub` has this form.
const TOMBSTONE_PREFIX: &str = "revoked:";

/// Serializes onboarding transactions, so an apply cannot slip a new row past a
/// concurrent revocation's tombstone check.
const ONBOARDING_LOCK: i64 = 0x5337_0b0a_2d00;

async fn begin(pool: &PgPool) -> anyhow::Result<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut tx = pool.begin().await.map_err(failed("begin"))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ONBOARDING_LOCK)
        .execute(&mut *tx)
        .await
        .map_err(failed("lock"))?;
    Ok(tx)
}

/// Upsert every membership in one transaction, refusing any reassignment.
pub async fn apply(
    pool: &PgPool,
    memberships: &[Membership],
    synthetic_only: bool,
) -> anyhow::Result<Vec<Outcome>> {
    validate(memberships, synthetic_only)?;
    let mut tx = begin(pool).await?;
    let mut outcomes = Vec::with_capacity(memberships.len());
    for m in memberships {
        let revoked_in: Option<Uuid> =
            sqlx::query_scalar("SELECT org_id FROM clinicians WHERE cognito_sub = $1")
                .bind(format!("{TOMBSTONE_PREFIX}{}", m.cognito_sub))
                .fetch_optional(&mut *tx)
                .await
                .map_err(failed("tombstone_lookup"))?;
        if let Some(org_id) = revoked_in {
            anyhow::ensure!(org_id == m.org_id, "subject_bound_to_another_org");
            anyhow::bail!("subject_revoked");
        }

        sqlx::query("INSERT INTO orgs (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
            .bind(m.org_id)
            .bind(&m.org_name)
            .execute(&mut *tx)
            .await
            .map_err(failed("org_insert"))?;
        let name: String = sqlx::query_scalar("SELECT name FROM orgs WHERE id = $1")
            .bind(m.org_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(failed("org_lookup"))?;
        // A mismatched name means the request names someone else's org id.
        anyhow::ensure!(name == m.org_name, "org_name_mismatch");

        let created: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO clinicians (id, org_id, cognito_sub, display_name)
             VALUES ($1, $2, $3, $4) ON CONFLICT (cognito_sub) DO NOTHING RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(m.org_id)
        .bind(&m.cognito_sub)
        .bind(&m.display_name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(failed("clinician_insert"))?;
        if created.is_some() {
            outcomes.push(Outcome::Created);
            continue;
        }

        let (id, org_id, display_name): (Uuid, Uuid, String) = sqlx::query_as(
            "SELECT id, org_id, display_name FROM clinicians WHERE cognito_sub = $1 FOR UPDATE",
        )
        .bind(&m.cognito_sub)
        .fetch_one(&mut *tx)
        .await
        .map_err(failed("clinician_lookup"))?;
        anyhow::ensure!(org_id == m.org_id, "subject_bound_to_another_org");
        if display_name == m.display_name {
            outcomes.push(Outcome::Unchanged);
        } else {
            sqlx::query("UPDATE clinicians SET display_name = $2 WHERE id = $1")
                .bind(id)
                .bind(&m.display_name)
                .execute(&mut *tx)
                .await
                .map_err(failed("clinician_rename"))?;
            outcomes.push(Outcome::Renamed);
        }
    }
    tx.commit().await.map_err(failed("commit"))?;
    Ok(outcomes)
}

/// End each subject's membership in the named org. The row keeps its id for
/// history and its org; its subject becomes `revoked:<sub>`, which no token can
/// carry. API tasks may still serve a cached membership until they are replaced.
pub async fn revoke(pool: &PgPool, revocations: &[Revocation]) -> anyhow::Result<Vec<Outcome>> {
    anyhow::ensure!(!revocations.is_empty(), "no revocations requested");
    for r in revocations {
        check_sub(&r.cognito_sub)?;
    }
    let mut tx = begin(pool).await?;
    let mut outcomes = Vec::with_capacity(revocations.len());
    for r in revocations {
        let bound: Option<Uuid> =
            sqlx::query_scalar("SELECT org_id FROM clinicians WHERE cognito_sub = $1 FOR UPDATE")
                .bind(&r.cognito_sub)
                .fetch_optional(&mut *tx)
                .await
                .map_err(failed("clinician_lookup"))?;
        match bound {
            None => outcomes.push(Outcome::Absent),
            Some(org_id) => {
                anyhow::ensure!(org_id == r.org_id, "subject_bound_to_another_org");
                sqlx::query("UPDATE clinicians SET cognito_sub = $2 WHERE cognito_sub = $1")
                    .bind(&r.cognito_sub)
                    .bind(format!("{TOMBSTONE_PREFIX}{}", r.cognito_sub))
                    .execute(&mut *tx)
                    .await
                    .map_err(failed("clinician_revoke"))?;
                outcomes.push(Outcome::Revoked);
            }
        }
    }
    tx.commit().await.map_err(failed("commit"))?;
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    //! Runs as the real onboarding grant: bootstrap the logins, migrate and
    //! converge as the owner, then onboard through the onboarding login only.
    use super::*;
    use crate::auth::resolver::ClinicianResolver;
    use crate::bootstrap::{self, Login, Logins};
    use crate::privileges;
    use sqlx::postgres::PgPoolOptions;

    const PASSWORD: &str = "synthetic-Password-0123456789-abcdef";
    const ORG_A: Uuid = Uuid::from_u128(0xA1);
    const ORG_B: Uuid = Uuid::from_u128(0xB2);

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

    /// The onboarding login's pool over a migrated, converged database.
    async fn onboarding_pool(pool: &PgPool) -> PgPool {
        let suffix = Uuid::now_v7().simple().to_string();
        let login = |role: &str| Login {
            username: format!("er_{role}_{suffix}"),
            password: PASSWORD.to_owned(),
        };
        let logins = Logins {
            owner: login("owner"),
            api: login("api"),
            worker: login("worker"),
            onboarding: login("onboarding"),
        };
        bootstrap::apply(pool, &logins).await.unwrap();
        let owner = connect_as(pool, &logins.owner).await;
        sqlx::migrate!("./migrations").run(&owner).await.unwrap();
        assert!(privileges::converge(&owner, true).await.unwrap());
        owner.close().await;
        connect_as(pool, &logins.onboarding).await
    }

    fn sub() -> String {
        Uuid::new_v4().to_string()
    }

    fn member(org_id: Uuid, org: &str, sub: &str, name: &str) -> Membership {
        Membership {
            org_id,
            org_name: format!("Synthetic {org}"),
            cognito_sub: sub.to_owned(),
            display_name: format!("Synthetic {name}"),
        }
    }

    async fn clinician_rows(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM clinicians")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrations = false)]
    async fn two_orgs_are_seeded_once_and_resolve_to_distinct_orgs(pool: PgPool) {
        let onboarding = onboarding_pool(&pool).await;
        let (a, b) = (sub(), sub());
        let request = [
            member(ORG_A, "Org A", &a, "Clinician A"),
            member(ORG_B, "Org B", &b, "Clinician B"),
        ];

        let first = apply(&onboarding, &request, true).await.unwrap();
        assert_eq!(first, [Outcome::Created, Outcome::Created]);
        let second = apply(&onboarding, &request, true).await.unwrap();
        assert_eq!(second, [Outcome::Unchanged, Outcome::Unchanged]);
        assert_eq!(clinician_rows(&pool).await, 2, "a rerun adds no rows");

        let resolver = ClinicianResolver::new(pool.clone());
        assert_eq!(resolver.resolve(&a).await.unwrap().org_id, ORG_A);
        assert_eq!(resolver.resolve(&b).await.unwrap().org_id, ORG_B);
    }

    #[sqlx::test(migrations = false)]
    async fn reassignment_is_refused_and_rolls_back_the_whole_request(pool: PgPool) {
        let onboarding = onboarding_pool(&pool).await;
        let a = sub();
        apply(
            &onboarding,
            &[member(ORG_A, "Org A", &a, "Clinician A")],
            true,
        )
        .await
        .unwrap();

        let fresh = sub();
        let transfer = [
            member(ORG_B, "Org B", &fresh, "Clinician B"),
            member(ORG_B, "Org B", &a, "Clinician A"),
        ];
        let err = apply(&onboarding, &transfer, true).await.unwrap_err();
        assert_eq!(err.to_string(), "subject_bound_to_another_org");

        let org: Uuid = sqlx::query_scalar("SELECT org_id FROM clinicians WHERE cognito_sub = $1")
            .bind(&a)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(org, ORG_A, "the clinician did not move");
        assert_eq!(
            clinician_rows(&pool).await,
            1,
            "the earlier entry rolled back"
        );
    }

    #[sqlx::test(migrations = false)]
    async fn an_existing_org_id_with_another_name_is_refused(pool: PgPool) {
        let onboarding = onboarding_pool(&pool).await;
        apply(&onboarding, &[member(ORG_A, "Org A", &sub(), "One")], true)
            .await
            .unwrap();
        let err = apply(&onboarding, &[member(ORG_A, "Org Z", &sub(), "Two")], true)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "org_name_mismatch");
    }

    #[sqlx::test(migrations = false)]
    async fn renaming_within_the_same_org_is_the_only_update(pool: PgPool) {
        let onboarding = onboarding_pool(&pool).await;
        let a = sub();
        apply(&onboarding, &[member(ORG_A, "Org A", &a, "Old")], true)
            .await
            .unwrap();
        let outcome = apply(&onboarding, &[member(ORG_A, "Org A", &a, "New")], true)
            .await
            .unwrap();
        assert_eq!(outcome, [Outcome::Renamed]);
    }

    #[sqlx::test(migrations = false)]
    async fn revocation_denies_resolution_and_keeps_history(pool: PgPool) {
        let onboarding = onboarding_pool(&pool).await;
        let a = sub();
        apply(
            &onboarding,
            &[member(ORG_A, "Org A", &a, "Clinician A")],
            true,
        )
        .await
        .unwrap();

        let wrong_org = [Revocation {
            org_id: ORG_B,
            cognito_sub: a.clone(),
        }];
        assert!(revoke(&onboarding, &wrong_org).await.is_err());

        let request = [Revocation {
            org_id: ORG_A,
            cognito_sub: a.clone(),
        }];
        assert_eq!(
            revoke(&onboarding, &request).await.unwrap(),
            [Outcome::Revoked]
        );
        assert_eq!(
            revoke(&onboarding, &request).await.unwrap(),
            [Outcome::Absent]
        );
        assert_eq!(
            clinician_rows(&pool).await,
            1,
            "the row survives for history"
        );

        // A fresh resolver stands in for a replaced API task; a running one
        // keeps its cached membership until the cache lifetime expires.
        let resolver = ClinicianResolver::new(pool.clone());
        assert!(resolver.resolve(&a).await.is_err());
    }

    #[sqlx::test(migrations = false)]
    async fn a_revoked_subject_is_neither_regranted_nor_moved(pool: PgPool) {
        let onboarding = onboarding_pool(&pool).await;
        let a = sub();
        let original = [member(ORG_A, "Org A", &a, "Clinician A")];
        apply(&onboarding, &original, true).await.unwrap();
        let revocation = [Revocation {
            org_id: ORG_A,
            cognito_sub: a.clone(),
        }];
        revoke(&onboarding, &revocation).await.unwrap();

        let err = apply(&onboarding, &original, true).await.unwrap_err();
        assert_eq!(err.to_string(), "subject_revoked");

        let elsewhere = [member(ORG_B, "Org B", &a, "Clinician A")];
        let err = apply(&onboarding, &elsewhere, true).await.unwrap_err();
        assert_eq!(err.to_string(), "subject_bound_to_another_org");

        assert_eq!(
            clinician_rows(&pool).await,
            1,
            "no second row for the subject"
        );
        let org: Uuid = sqlx::query_scalar("SELECT org_id FROM clinicians WHERE cognito_sub = $1")
            .bind(format!("{TOMBSTONE_PREFIX}{a}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(org, ORG_A, "the tombstone keeps the original org");
        let orgs: i64 = sqlx::query_scalar("SELECT count(*) FROM orgs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(orgs, 1, "the refused request rolled back its org insert");
        let resolver = ClinicianResolver::new(pool.clone());
        assert!(resolver.resolve(&a).await.is_err());
    }

    #[test]
    fn request_validation_rejects_unsafe_input() {
        let a = sub();
        let ok = member(ORG_A, "Org A", &a, "Clinician");
        validate(std::slice::from_ref(&ok), true).unwrap();

        let username = Membership {
            cognito_sub: "synthetic-clinician-a".into(),
            ..ok.clone()
        };
        let upper = Membership {
            cognito_sub: a.to_uppercase(),
            ..ok.clone()
        };
        let real_name = Membership {
            display_name: "Jane Doe".into(),
            ..ok.clone()
        };
        for bad in [username, upper, real_name] {
            assert!(validate(&[bad], true).is_err());
        }
        assert!(
            validate(&[ok.clone(), ok.clone()], true).is_err(),
            "duplicate subject"
        );
        assert!(validate(&[], true).is_err());
        let prod_name = Membership {
            display_name: "Clinician Name".into(),
            ..ok
        };
        validate(&[prod_name], false).unwrap();
    }
}
