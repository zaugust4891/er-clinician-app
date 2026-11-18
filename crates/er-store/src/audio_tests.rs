use crate::{
    SqliteRepository, StoreError,
    audio::{AudioChunk, ChunkState},
    db::open_memory_db,
};
use uuid::Uuid;

fn repo() -> SqliteRepository {
    SqliteRepository::new(open_memory_db().unwrap())
}
fn chunk(patient: Uuid, segment: Uuid, position: u32, index: Option<u32>) -> AudioChunk {
    AudioChunk {
        chunk_id: Uuid::now_v7(),
        patient_id: patient,
        segment_id: segment,
        position,
        chunk_index: index,
        file_name: format!("{segment}_{position}_0_1.wav"),
        content_type: "audio/wav".into(),
        byte_len: 48,
        sha256: "00".repeat(32),
        started_at_ms: 1_000 * position as i64,
        ended_at_ms: 1_000 * position as i64 + 999,
        is_final: false,
        state: if index.is_some() {
            ChunkState::Queued
        } else {
            ChunkState::Rejected
        },
        reason: index.is_none().then(|| "synthetic".into()),
    }
}

#[test]
fn rows_are_unique_per_file_position_and_index() {
    let r = repo();
    let (p, s) = (Uuid::now_v7(), Uuid::now_v7());
    let first = chunk(p, s, 0, Some(0));
    r.insert_audio_chunk(&first).unwrap();
    assert!(r.audio_file_known(&first.file_name).unwrap());
    // Same recorder position, same server index, and the same file are all refused.
    let mut again = chunk(p, s, 0, Some(1));
    again.file_name = "other.wav".into();
    assert!(r.insert_audio_chunk(&again).is_err());
    let mut index_taken = chunk(p, s, 1, Some(0));
    index_taken.file_name = "another.wav".into();
    assert!(r.insert_audio_chunk(&index_taken).is_err());
    let mut same_file = chunk(p, s, 2, Some(2));
    same_file.file_name = first.file_name.clone();
    assert!(r.insert_audio_chunk(&same_file).is_err());
    // New rows can never claim finality or a later state directly.
    let mut final_row = chunk(p, s, 3, Some(3));
    final_row.is_final = true;
    assert!(matches!(
        r.insert_audio_chunk(&final_row),
        Err(StoreError::Integrity)
    ));
}

#[test]
fn sealing_marks_exactly_the_highest_index_and_gates_work() {
    let r = repo();
    let (p, s) = (Uuid::now_v7(), Uuid::now_v7());
    for (position, index) in [(0, Some(0)), (1, None), (2, Some(1)), (3, Some(2))] {
        r.insert_audio_chunk(&chunk(p, s, position, index)).unwrap();
    }
    // An unsealed segment is still recording: nothing may upload yet.
    assert_eq!(r.next_audio_work(p).unwrap(), None);
    assert!(r.seal_segment(s).unwrap());
    assert!(r.seal_segment(s).unwrap());
    let rows = r.segment_chunks(s).unwrap();
    let finals: Vec<_> = rows.iter().filter(|c| c.is_final).collect();
    assert_eq!(finals.len(), 1);
    assert_eq!(finals[0].chunk_index, Some(2));
    assert_eq!(r.next_audio_work(p).unwrap().unwrap().chunk_index, Some(0));
    // A segment with no accepted audio cannot be sealed.
    let empty = Uuid::now_v7();
    r.insert_audio_chunk(&chunk(p, empty, 0, None)).unwrap();
    assert!(!r.seal_segment(empty).unwrap());
}

#[test]
fn states_move_by_compare_and_set_and_rejection_keeps_the_index() {
    let r = repo();
    let (p, s) = (Uuid::now_v7(), Uuid::now_v7());
    let a = chunk(p, s, 0, Some(0));
    let b = chunk(p, s, 1, Some(1));
    r.insert_audio_chunk(&a).unwrap();
    r.insert_audio_chunk(&b).unwrap();
    r.seal_segment(s).unwrap();
    use ChunkState::*;
    r.set_audio_state(a.chunk_id, &[Queued], Uploaded, None)
        .unwrap();
    assert!(matches!(
        r.set_audio_state(a.chunk_id, &[Queued], Uploaded, None),
        Err(StoreError::Conflict)
    ));
    r.set_audio_state(a.chunk_id, &[Uploaded], Completed, None)
        .unwrap();
    // Work moves to the next index once the first is completed.
    assert_eq!(r.next_audio_work(p).unwrap().unwrap().chunk_id, b.chunk_id);
    assert!(matches!(
        r.set_audio_state(a.chunk_id, &[Completed], Rejected, Some("late")),
        Err(StoreError::Integrity)
    ));
    r.set_audio_state(
        b.chunk_id,
        &[Queued, Uploaded],
        Rejected,
        Some("file_changed"),
    )
    .unwrap();
    let rejected = &r.segment_chunks(s).unwrap()[1];
    assert_eq!(
        (rejected.chunk_index, rejected.reason.as_deref()),
        (Some(1), Some("file_changed"))
    );
    assert_eq!(r.next_audio_work(p).unwrap(), None);
    let counts = r.audio_counts().unwrap();
    assert_eq!((counts.completed, counts.rejected), (1, 1));
}
