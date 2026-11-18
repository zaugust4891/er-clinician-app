//! Phase 5 Step 4 on the local stack: a phone's chunk files travel through the
//! real API, object storage (RustFS), the queue and the worker, and the
//! assembled transcript comes back to the phone, which then deletes its audio.
//! A file-writing recorder stands in for the iOS one; the echo transcriber
//! turns each WAV's payload back into its words.
mod common;
use common::*;
use er_ai::summarizer::{AppSummarizer, MockSummarizer};
use er_device::{
    AccessTokenProvider, AudioView, CaptureAdapter, ConsentStatus, DeviceConfig, DeviceHandle,
    SyncConfig,
    audio::{PARTIAL_SUFFIX, chunk_file_name, synthetic_wav},
};
use er_domain::NoteEvent;
use er_pipeline::speech_to_text::{AppSpeechToText, MockSpeechToText};
use er_server::{
    repository::PostgresRepository,
    worker::{self, WorkerCtx},
};
use er_store::Repository;
use sqlx::PgPool;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const MIN: i64 = 60_000;
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

struct Token(String);
impl AccessTokenProvider for Token {
    fn access_token(&self, _: bool) -> er_device::Result<String> {
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct RecorderState {
    dir: Option<PathBuf>,
    segment: Option<Uuid>,
    position: u32,
    trailing: Vec<(i64, i64, &'static str)>,
}
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<RecorderState>>);
impl Recorder {
    fn chunk(&self, start: i64, end: i64, words: &str) {
        let mut s = self.0.lock().unwrap();
        let name =
            chunk_file_name(s.segment.unwrap(), s.position, start, end, "audio/wav").unwrap();
        s.position += 1;
        let dir = s.dir.clone().unwrap();
        let partial = dir.join(format!("{name}{PARTIAL_SUFFIX}"));
        std::fs::write(&partial, synthetic_wav(words.as_bytes())).unwrap();
        std::fs::rename(partial, dir.join(name)).unwrap();
    }
    fn trailing(&self, start: i64, end: i64, words: &'static str) {
        self.0.lock().unwrap().trailing.push((start, end, words));
    }
}
impl CaptureAdapter for Recorder {
    fn available(&self) -> bool {
        true
    }
    fn start(&mut self, _: Uuid, segment: Uuid, _: bool, dir: &Path) -> er_device::Result<()> {
        let mut s = self.0.lock().unwrap();
        (s.dir, s.segment, s.position) = (Some(dir.into()), Some(segment), 0);
        Ok(())
    }
    fn stop(&mut self) -> er_device::Result<()> {
        let trailing = std::mem::take(&mut self.0.lock().unwrap().trailing);
        for (start, end, words) in trailing {
            self.chunk(start, end, words);
        }
        Ok(())
    }
}

struct Phone {
    handle: Arc<DeviceHandle>,
    path: PathBuf,
    recorder: Recorder,
}
impl Phone {
    fn config(path: &Path) -> DeviceConfig {
        DeviceConfig {
            database_path: path.to_str().unwrap().into(),
            account_scope: "synthetic-clinician".into(),
        }
    }
    fn open() -> Self {
        let path = std::env::temp_dir().join(format!("er-audio-sync-{}.sqlite", Uuid::now_v7()));
        let recorder = Recorder::default();
        let handle =
            DeviceHandle::open_with_capture(Self::config(&path), Box::new(recorder.clone()))
                .unwrap();
        Self {
            handle: Arc::new(handle),
            path,
            recorder,
        }
    }
    /// Close, edit the SQLite file directly (a simulated lost reply), reopen.
    fn reopen_after(&mut self, sql: &str) {
        self.handle.close().unwrap();
        rusqlite::Connection::open(&self.path)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        self.handle = Arc::new(
            DeviceHandle::open_with_capture(
                Self::config(&self.path),
                Box::new(self.recorder.clone()),
            )
            .unwrap(),
        );
    }
    fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut er_device::DeviceCore) -> er_device::Result<T> + Send + 'static,
    ) -> T {
        self.handle.execute(true, f).unwrap()
    }
    fn audio(&self) -> AudioView {
        self.run(|c| c.audio_status())
    }
    /// Every byte SQLite holds for this phone, including the write-ahead log.
    fn database_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for suffix in ["", "-wal"] {
            bytes.extend(
                std::fs::read(format!("{}{}", self.path.display(), suffix)).unwrap_or_default(),
            );
        }
        bytes
    }
}
impl Drop for Phone {
    fn drop(&mut self) {
        let _ = self.handle.close();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.path.display(), suffix));
        }
        let _ = std::fs::remove_dir_all(format!("{}.audio", self.path.display()));
    }
}

async fn settle(phone: &Phone, config: &SyncConfig, token: &str) {
    for _ in 0..12 {
        let handle = phone.handle.clone();
        let config = config.clone();
        let token = Arc::new(Token(token.into()));
        let state = tokio::task::spawn_blocking(move || handle.synchronize(config, token))
            .await
            .unwrap()
            .unwrap();
        if state.status == "up_to_date" {
            return;
        }
        assert_eq!(state.status, "pending", "unexpected synchronization state");
    }
    panic!("sync did not finish within the bounded test rounds");
}

fn echo_worker(pool: PgPool, audio: &AudioFixture) -> WorkerCtx {
    WorkerCtx {
        pool,
        s3: audio.s3.clone(),
        sqs: audio.sqs.clone(),
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText { echo: true }),
        summarizer: AppSummarizer::Mock(MockSummarizer::new()),
        wait_time_seconds: 0,
        limits: er_server::config::WorkerLimits {
            visibility_seconds: 0,
            ..Default::default()
        },
    }
}
async fn drain(ctx: &WorkerCtx) {
    while worker::poll_once(ctx).await.expect("a poll cycle") > 0 {}
}

async fn setup(pool: &PgPool, sub: &str) -> (AudioFixture, SyncConfig, String) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(pool, ORG_A, sub).await;
    let origin = spawn_server(er_server::routes::router(
        test_state_with_audio(pool.clone(), &audio).await,
    ))
    .await;
    let clinician: Uuid = sqlx::query_scalar("SELECT id FROM clinicians WHERE org_id=$1")
        .bind(ORG_A)
        .fetch_one(pool)
        .await
        .unwrap();
    let config = SyncConfig {
        origin,
        organization: ORG_A,
        clinician,
    };
    (audio, config, mint(sub))
}

/// `(patient, chunk_index, is_final, status)` per encounter, in index order.
async fn server_chunks(pool: &PgPool, encounter: Uuid) -> Vec<(Uuid, i32, bool, String)> {
    sqlx::query_as(
        "SELECT patient_id,chunk_index,is_final,status FROM audio_chunks WHERE encounter_id=$1 ORDER BY chunk_index",
    )
    .bind(encounter)
    .fetch_all(pool)
    .await
    .unwrap()
}
async fn transcripts(pool: &PgPool, patient: Uuid) -> Vec<(Uuid, String)> {
    PostgresRepository::for_org(pool.clone(), ORG_A)
        .get_events(patient)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            NoteEvent::TranscriptReady {
                encounter_id, text, ..
            } => Some((encounter_id, text)),
            _ => None,
        })
        .collect()
}

#[sqlx::test(migrations = "./migrations")]
async fn phone_audio_reaches_the_note_through_the_real_pipeline(pool: PgPool) {
    let (audio, config, token) = setup(&pool, "audio-phone").await;
    let phone = Phone::open();
    let t0 = now() - 30 * MIN;
    let (a, b) = phone.run(move |c| {
        c.start_shift(t0)?;
        let a = er_device::core::id(&c.add_patient("Synthetic A".into(), None, t0)?)?;
        let b = er_device::core::id(&c.add_patient("Synthetic B".into(), None, t0)?)?;
        c.select_patient(Some(a), t0)?;
        c.set_consent(a, ConsentStatus::Granted, "synthetic-v1".into(), 0, t0)?;
        Ok((a, b))
    });
    let segment = |s: String| Uuid::parse_str(&s).unwrap();

    // Patient A, ambient: three chunks, the last flushed by the pause.
    let a1 = segment(phone.run(move |c| c.start_capture(a, t0 + MIN)));
    phone.recorder.chunk(t0 + MIN, t0 + 2 * MIN, "alpha");
    phone.recorder.chunk(t0 + 2 * MIN, t0 + 3 * MIN, "bravo");
    phone.run(|c| c.collect_audio());
    phone
        .recorder
        .trailing(t0 + 3 * MIN, t0 + 3 * MIN + 20_000, "charlie");
    phone.run(move |c| c.pause(a, t0 + 3 * MIN + 20_000, "user_pause".into()));
    // Resume A, then switch to B: the switch is A's second boundary.
    let a2 = segment(phone.run(move |c| c.start_capture(a, t0 + 4 * MIN)));
    phone.recorder.trailing(t0 + 4 * MIN, t0 + 5 * MIN, "delta");
    phone.run(move |c| c.select_patient(Some(b), t0 + 5 * MIN));
    // Patient B, solo dictation.
    let b1 = segment(phone.run(move |c| {
        c.set_mode(b, true)?;
        c.start_capture(b, t0 + 6 * MIN)
    }));
    phone.recorder.chunk(t0 + 6 * MIN, t0 + 7 * MIN, "echo");
    phone
        .recorder
        .trailing(t0 + 7 * MIN, t0 + 7 * MIN + 5_000, "foxtrot");
    phone.run(move |c| c.pause(b, t0 + 7 * MIN + 5_000, "user_pause".into()));
    assert_eq!(phone.audio().awaiting_upload, 6);

    settle(&phone, &config, &token).await;
    let view = phone.audio();
    assert_eq!((view.awaiting_upload, view.awaiting_transcript), (0, 6));
    for (encounter, patient, finals) in [(a1, a, 2), (a2, a, 0), (b1, b, 1)] {
        let rows = server_chunks(&pool, encounter).await;
        assert_eq!(rows.len(), finals + 1, "one server row per local chunk");
        for (n, (owner, index, is_final, status)) in rows.iter().enumerate() {
            assert_eq!(*owner, patient, "no chunk crosses a patient");
            assert_eq!((*index, *is_final), (n as i32, n == finals));
            assert_eq!(status, "uploaded");
        }
    }
    // The presigned URL is a bearer credential: it never reached SQLite.
    let stored = phone.database_bytes();
    for needle in [&b"X-Amz-Signature"[..], b"X-Amz-Credential", b"upload_url"] {
        assert!(
            !stored.windows(needle.len()).any(|w| w == needle),
            "SQLite holds {}",
            String::from_utf8_lossy(needle)
        );
    }

    drain(&echo_worker(pool.clone(), &audio)).await;
    let mut a_text = transcripts(&pool, a).await;
    a_text.sort();
    let mut expected = vec![
        (a1, "alpha\n\nbravo\n\ncharlie".to_string()),
        (a2, "delta".to_string()),
    ];
    expected.sort();
    assert_eq!(a_text, expected);
    assert_eq!(
        transcripts(&pool, b).await,
        [(b1, "echo\n\nfoxtrot".to_string())]
    );

    // The transcript arriving on the phone is what lets it delete its audio.
    settle(&phone, &config, &token).await;
    let view = phone.audio();
    assert_eq!((view.awaiting_transcript, view.processed), (0, 6));
    let dir = PathBuf::from(&view.directory);
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
    let encounters = phone.run(move |c| Ok(c.note_view(a)?.encounter_count));
    assert_eq!(encounters, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn lost_replies_retry_with_the_same_chunk_identity(pool: PgPool) {
    let (audio, config, token) = setup(&pool, "audio-retry").await;
    let mut phone = Phone::open();
    let t0 = now() - 30 * MIN;
    let p = phone.run(move |c| {
        c.start_shift(t0)?;
        let p = er_device::core::id(&c.add_patient("Synthetic".into(), None, t0)?)?;
        c.select_patient(Some(p), t0)?;
        c.set_mode(p, true)?;
        Ok(p)
    });
    let first = Uuid::parse_str(&phone.run(move |c| c.start_capture(p, t0 + MIN))).unwrap();
    phone.recorder.chunk(t0 + MIN, t0 + 2 * MIN, "one");
    phone.recorder.trailing(t0 + 2 * MIN, t0 + 3 * MIN, "two");
    phone.run(move |c| c.pause(p, t0 + 3 * MIN, "user_pause".into()));
    let second = Uuid::parse_str(&phone.run(move |c| c.start_capture(p, t0 + 4 * MIN))).unwrap();
    phone.recorder.trailing(t0 + 4 * MIN, t0 + 5 * MIN, "three");
    phone.run(move |c| c.pause(p, t0 + 5 * MIN, "user_pause".into()));

    // Before the first sync, pretend the second segment's PUT reply was lost
    // after a PUT that never happened: completion finds no row and re-uploads.
    phone.reopen_after(&format!(
        "UPDATE device_audio_chunks SET state='uploaded' WHERE segment_id='{second}'"
    ));
    settle(&phone, &config, &token).await;
    assert_eq!(phone.audio().awaiting_transcript, 3);
    assert_eq!(server_chunks(&pool, second).await.len(), 1);

    // Now lose both replies for the first segment after the server acted on
    // them: a lost completion and a lost upload. Neither mints a new chunk.
    phone.reopen_after(&format!(
        "UPDATE device_audio_chunks SET state='uploaded' WHERE segment_id='{first}' AND chunk_index=0;
         UPDATE device_audio_chunks SET state='queued' WHERE segment_id='{first}' AND chunk_index=1;"
    ));
    assert_eq!(phone.audio().awaiting_upload, 2);
    settle(&phone, &config, &token).await;
    assert_eq!(phone.audio().awaiting_transcript, 3);
    let rows = server_chunks(&pool, first).await;
    assert_eq!(
        rows.iter().map(|r| (r.1, r.2)).collect::<Vec<_>>(),
        [(0, false), (1, true)]
    );

    drain(&echo_worker(pool.clone(), &audio)).await;
    let mut text = transcripts(&pool, p).await;
    text.sort();
    let mut expected = vec![
        (first, "one\n\ntwo".to_string()),
        (second, "three".to_string()),
    ];
    expected.sort();
    assert_eq!(
        text, expected,
        "a replayed PUT cannot change completed audio"
    );
    settle(&phone, &config, &token).await;
    assert_eq!(phone.audio().processed, 3);
}
