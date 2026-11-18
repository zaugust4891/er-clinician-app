use crate::{Repository, SqliteRepository, StoreError, db::init_db, device::DeviceState};
use rusqlite::Connection;
use uuid::Uuid;

#[test]
fn legacy_upgrade_preserves_events_and_repeated_migrations() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE events(sequence_id INTEGER PRIMARY KEY AUTOINCREMENT,event_id TEXT NOT NULL UNIQUE,patient_id TEXT NOT NULL,event_type TEXT NOT NULL,payload TEXT NOT NULL,occurred_at TEXT NOT NULL);CREATE TABLE snapshots(patient_id TEXT PRIMARY KEY,sequence_id INTEGER NOT NULL,snapshot TEXT NOT NULL,checksum TEXT NOT NULL,created_at TEXT NOT NULL); INSERT INTO events(event_id,patient_id,event_type,payload,occurred_at) VALUES('original','original','original','original','original');").unwrap();
    init_db(&conn).unwrap();
    init_db(&conn).unwrap();
    assert_eq!(
        conn.query_row("SELECT payload FROM events", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "original"
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.pragma_query_value(None, "synchronous", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
}
#[test]
fn failed_migration_rolls_back_and_future_schema_is_rejected() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE VIEW device_state AS SELECT 1;")
        .unwrap();
    assert!(init_db(&conn).is_err());
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='device_scope'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    conn.pragma_update(None, "user_version", 99).unwrap();
    assert!(matches!(init_db(&conn), Err(StoreError::Schema)));
}
#[test]
fn stale_save_scope_and_failed_transaction_preserve_acknowledged_state() {
    let repo = SqliteRepository::new(crate::db::open_memory_db().unwrap());
    let original = repo.bind_device_scope("account-a").unwrap();
    assert!(matches!(
        repo.bind_device_scope("account-b"),
        Err(StoreError::Scope)
    ));
    let mut next = original.clone();
    next.revision = 1;
    repo.commit_device_change(0, &next, None).unwrap();
    assert!(matches!(
        repo.commit_device_change(0, &next, None),
        Err(StoreError::Conflict)
    ));
    repo.execute_raw("CREATE TRIGGER fail_state BEFORE UPDATE ON device_state BEGIN SELECT RAISE(FAIL,'synthetic'); END",&[]);
    let mut third = next.clone();
    third.revision = 2;
    assert!(repo.commit_device_change(1, &third, None).is_err());
    assert_eq!(repo.bind_device_scope("account-a").unwrap().revision, 1);
    assert_eq!(DeviceState::default().format_version, 1);
}
#[tokio::test]
async fn duplicate_ids_must_match_patient_and_body() {
    let repo = SqliteRepository::new(crate::db::open_memory_db().unwrap());
    let p = Uuid::now_v7();
    let id = Uuid::now_v7();
    let e = er_domain::NoteEvent::data_reviewed(p, "synthetic".into());
    let seq = repo.append_event(p, id, e.clone()).await.unwrap();
    assert_eq!(repo.append_event(p, id, e).await.unwrap(), seq);
    assert!(matches!(
        repo.append_event(
            p,
            id,
            er_domain::NoteEvent::data_reviewed(p, "changed".into())
        )
        .await,
        Err(StoreError::Conflict)
    ));
}

#[test]
fn full_database_preserves_the_last_acknowledged_metadata() {
    use crate::device::{Definition, DefinitionKind};
    let repo = SqliteRepository::new(crate::db::open_memory_db().unwrap());
    let mut next = repo.bind_device_scope("synthetic-account").unwrap();
    let conn = repo.conn.lock().unwrap();
    let pages: u32 = conn
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    conn.pragma_update(None, "max_page_count", pages).unwrap();
    drop(conn);
    next.revision = 1;
    next.definitions.push(Definition {
        id: Uuid::now_v7(),
        version: 1,
        name: "Synthetic".into(),
        kind: DefinitionKind::Custom,
        instructions: "x".repeat(8192),
        sections: vec![],
        archived: false,
        is_default: false,
    });
    assert!(
        matches!(repo.commit_device_change(0,&next,None),Err(StoreError::Db(rusqlite::Error::SqliteFailure(ref e,_))) if e.code==rusqlite::ErrorCode::DiskFull)
    );
    assert_eq!(
        repo.bind_device_scope("synthetic-account")
            .unwrap()
            .revision,
        0
    );
}
