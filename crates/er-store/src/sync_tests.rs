use super::*;
use crate::sync::*;
use uuid::Uuid;
fn repo() -> SqliteRepository {
    let r = SqliteRepository::new(db::open_memory_db().unwrap());
    r.bind_device_scope("synthetic").unwrap();
    r
}
#[tokio::test]
async fn acknowledgments_do_not_advance_cursor_and_partial_retry_keeps_ids() {
    let r = repo();
    let p = Uuid::now_v7();
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let first = er_domain::NoteEvent::data_reviewed(p, "first".into());
    let second = er_domain::NoteEvent::data_reviewed(p, "second".into());
    r.append_event(p, a, first.clone()).await.unwrap();
    r.append_event(p, b, second).await.unwrap();
    let ledger = SyncLedger::default();
    r.acknowledge_events(p, &[a, b], &[(a, 1u64 << 54)], &ledger)
        .unwrap();
    assert_eq!(r.delivery_counts(p).unwrap().queued, 1);
    assert_eq!(r.pending_events(p).unwrap()[0].event_id, b);
    assert_eq!(r.delivery_counts(p).unwrap().awaiting_download, 1);
    assert!(!r.sync_ledger().unwrap().patients.contains_key(&p));
    r.receive_page(
        p,
        0,
        &[RemoteEvent {
            sequence: 1u64 << 54,
            id: a,
            event: first,
        }],
        false,
        ledger,
    )
    .unwrap();
    assert_eq!(r.delivery_counts(p).unwrap().awaiting_download, 0);
    assert_eq!(r.sync_ledger().unwrap().patients[&p].cursor, 1u64 << 54);
}
#[tokio::test]
async fn page_transaction_checks_bodies_identity_order_and_preserves_cursor_on_failure() {
    let r = repo();
    let p = Uuid::now_v7();
    let id = Uuid::now_v7();
    let e = er_domain::NoteEvent::data_reviewed(p, "synthetic".into());
    r.append_event(p, id, e.clone()).await.unwrap();
    let invalid = RemoteEvent {
        sequence: 10,
        id,
        event: er_domain::NoteEvent::data_reviewed(p, "changed body".into()),
    };
    assert!(
        r.receive_page(p, 0, &[invalid], false, SyncLedger::default())
            .is_err()
    );
    assert!(!r.sync_ledger().unwrap().patients.contains_key(&p));
    assert!(
        r.receive_page(p, 0, &[], true, SyncLedger::default())
            .is_err()
    );
    r.execute_raw("CREATE TRIGGER fail_cursor BEFORE INSERT ON device_sync BEGIN SELECT RAISE(FAIL,'synthetic'); END",&[]);
    let remote = RemoteEvent {
        sequence: 10,
        id,
        event: e,
    };
    assert!(
        r.receive_page(
            p,
            0,
            std::slice::from_ref(&remote),
            false,
            SyncLedger::default()
        )
        .is_err()
    );
    assert_eq!(r.delivery_counts(p).unwrap().queued, 1);
    r.execute_raw("DROP TRIGGER fail_cursor", &[]);
    r.receive_page(p, 0, &[remote], false, SyncLedger::default())
        .unwrap();
    assert_eq!(r.sync_ledger().unwrap().patients[&p].cursor, 10);
}
#[tokio::test]
async fn server_order_precedes_pending_tail_and_snapshots_are_disabled() {
    let r = repo();
    let p = Uuid::now_v7();
    let local = Uuid::now_v7();
    let local_event = er_domain::NoteEvent::data_reviewed(p, "pending".into());
    r.append_event(p, local, local_event).await.unwrap();
    r.save_snapshot(p, 1, &er_domain::PatientNote::new(p))
        .await
        .unwrap();
    assert!(r.get_latest_snapshot(p).await.unwrap().is_some());
    let ledger = SyncLedger {
        binding: Some(SyncBinding {
            origin: "https://synthetic.invalid".into(),
            organization: Uuid::now_v7(),
            clinician: Uuid::now_v7(),
        }),
        ..Default::default()
    };
    r.save_sync_ledger(&ledger).unwrap();
    assert!(r.get_latest_snapshot(p).await.unwrap().is_none());
    let remote = Uuid::now_v7();
    r.receive_page(
        p,
        0,
        &[RemoteEvent {
            sequence: 30,
            id: remote,
            event: er_domain::NoteEvent::data_reviewed(p, "server first".into()),
        }],
        false,
        ledger,
    )
    .unwrap();
    let events = r.get_events(p).await.unwrap();
    assert_eq!(
        events.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        [remote, local]
    );
    r.mark_sync_conflict(local, "remote_state_changed").unwrap();
    assert_eq!(r.delivery_counts(p).unwrap().conflicts, 1);
    assert_eq!(r.get_events(p).await.unwrap().len(), 1);
    r.discard_sync_conflict(p, local).unwrap();
    assert_eq!(r.delivery_counts(p).unwrap().conflicts, 0);
}

#[tokio::test]
async fn step_two_upgrade_backfills_the_existing_event_ids() {
    let r = repo();
    let p = Uuid::now_v7();
    let event = Uuid::now_v7();
    r.append_event(
        p,
        event,
        er_domain::NoteEvent::data_reviewed(p, "saved in Step 2".into()),
    )
    .await
    .unwrap();
    {
        let conn = r.conn.lock().unwrap();
        conn.execute_batch("DROP TRIGGER device_queue_event; DROP TABLE device_event_delivery; DROP TABLE device_sync; PRAGMA user_version=1;").unwrap();
        db::init_db(&conn).unwrap();
    }
    assert_eq!(r.pending_events(p).unwrap()[0].event_id, event);
    assert_eq!(r.get_events(p).await.unwrap().len(), 1);
}
