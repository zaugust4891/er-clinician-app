# Phase 4 — Step 3 Deep Dive (Async `Repository` + `PostgresRepository` + `event_id`)

*Companion to `phase4-implementation-guide.md` §Step 3. The guide tells you **what** to type;
this document explains **why each change exists and what concept it teaches**, grounded in the
exact files in this repo. Read this once before you touch code, then keep the guide open as the
checklist. Nothing here is copy-paste-complete — it is meant to make every line of Step 3
unsurprising before you write it.*

---

## 0. The thesis: what Step 3 actually is

Step 3 is **one refactor wearing three hats.** They look like three tasks but they are the same
task seen from three angles:

1. **Make the persistence layer asynchronous.** Today every database call blocks the calling
   thread until SQLite answers. That is fine for a desktop CLI. It is fatal inside a web server.
2. **Add a second implementation of that layer that talks to PostgreSQL** (via a library called
   `sqlx`). The cloud needs a real shared database, not a per-device SQLite file.
3. **Give every event a stable identity (`event_id`) that the *client* assigns**, so that when a
   phone uploads the same event twice (because the network hiccuped), the server stores it exactly
   once.

Why are these one task? Because the thing that connects them is **the `Repository` trait**
(`crates/er-store/src/repository.rs:8`). It is the seam — the single interface — that the rest of
the application talks to when it wants to read or write events. If you change that seam's *shape*
(sync → async, and add an `event_id` parameter), the change ripples outward to every caller. Once
the seam is async and identity-aware, dropping in a Postgres implementation is "just" writing one
more `impl` block. So the hard, careful work is the seam change; Postgres is the payoff.

The reason this is "the load-bearing refactor of the whole phase" (guide §3.1) is that **Steps
4–7 all assume an async server.** Auth middleware, the sync API, the SQS worker, server-side
AI — every one of them runs inside the async web server you skeletoned in Step 2. If the
persistence layer were still synchronous, those steps could not call it without the program
crashing (you'll see exactly why in §2). So Step 3 is the gate everything else walks through.

---

## 1. Foundational concept: synchronous vs. asynchronous I/O

You cannot understand *why* the trait must become async without understanding what "async" buys
you. So start here, even if it feels basic — the rest of the step is mechanical once this clicks.

### 1.1 What "blocking" means

When your code calls `conn.execute("INSERT …")` in `SqliteRepository::append_event`
(`repository.rs:64`), the operating system thread running that code **stops and waits** until
SQLite finishes writing to disk and returns. The thread is "blocked." It is not doing anything
else during that wait — it is parked, holding its place in line. The CPU is free to run *other*
threads, but *this* thread is stuck.

For the CLI this is completely fine. There is one user, one thread doing one thing at a time,
and SQLite is local and fast (microseconds). Blocking a thread that has nothing else to do costs
nothing.

### 1.2 Why blocking is fatal in a server

A web server is the opposite situation. It must serve **many requests at once** — fifty
clinicians syncing events simultaneously. The naive model is "one operating-system thread per
request." That breaks down because:

- A database query over the network can take *milliseconds to seconds*. During that time the
  thread is blocked, doing nothing, but still consuming memory (each thread reserves a stack,
  often ~1–8 MB).
- You cannot have tens of thousands of blocked threads. The machine runs out of memory and the
  OS scheduler thrashes switching between them.

**Asynchronous I/O** solves this. Instead of "block the thread until the database answers," the
model becomes "**start** the database call, and while waiting for the answer, **let this thread
go run other work**; come back and finish when the answer arrives." A handful of threads can
juggle thousands of in-flight requests, because no thread ever sits idle waiting.

### 1.3 What `async`/`.await` actually are in Rust

In Rust the mechanism is built from three pieces:

- **`async fn`** — marking a function `async` means: *calling it does not run it.* Instead it
  returns a **`Future`** — a value that represents "a computation that will eventually produce a
  result, but hasn't yet." Think of a `Future` as a pausable, resumable state machine. Calling the
  function just constructs the machine; it does no work.
- **`.await`** — this is how you actually drive a `Future` toward its result. When you write
  `repo.append_event(…).await`, you are saying "run this future; if it needs to wait for I/O,
  **pause my function here and hand control back**, and resume me right at this line when the I/O
  is ready." The pause is the magic: it frees the thread to do other things.
- **The runtime (executor)** — `.await` and `Future`s are inert by themselves. Something has to
  actually poll the state machines, notice when I/O is ready, and resume the right futures. That
  something is the **async runtime**. In this codebase it is **Tokio** (`tokio` in
  `Cargo.toml:20`). When you write `#[tokio::main]` on a function, you are wrapping it in a Tokio
  runtime that owns a small pool of worker threads and schedules all your futures across them.

The key mental shift: **`async` is cooperative.** A future runs until it hits an `.await` on
something not-yet-ready, then *voluntarily yields* the thread. This is why one thread can serve
many requests — they take turns at their `.await` points.

### 1.4 Why `sqlx` forces async on you, and why you can't dodge it

The Postgres client you will use, **`sqlx`**, is async to its core. Its query methods *are*
`async fn`s and return futures. There is no synchronous "just block here" API. This is a
deliberate design choice by `sqlx`: a Postgres connection is a network socket, and network I/O
is exactly the slow-wait that async exists to handle.

So the moment you want a `PostgresRepository`, its `append_event` body must `.await` a `sqlx`
call. And a function that contains `.await` must itself be `async`. That requirement climbs up
the call chain: the trait method must be async, every caller of it must be async (or bridge to
async), and so on. **This is "the exact ripple list" in guide §3.1.** It is not optional polish;
it is forced by `sqlx` being async.

### 1.5 The one thing you must never do: `block_on` inside the server

There is a tempting shortcut: "what if I keep the trait synchronous, and inside
`PostgresRepository` I call `runtime.block_on(sqlx_call)` to *force* the async call to finish
before returning?" `block_on` means "run this future to completion right now, blocking this
thread until it's done" — it is the bridge from sync code into async code.

**This shortcut crashes the server.** Here is precisely why. Your axum web handlers already run
*on Tokio's worker threads* (axum is built on Tokio; `#[tokio::main]` in
`crates/er-server/src/main.rs:13` set that up). Calling `block_on` from a thread that is *already*
a Tokio worker tells Tokio: "park this worker thread and don't let it do anything until this
future finishes." But that future may itself need that very worker thread to make progress.
Tokio detects this deadlock-in-waiting at runtime and **panics**:

> `Cannot start a runtime from within a runtime. This happens because a function (like block_on)
> attempted to block the current thread while the thread is being used to drive asynchronous tasks.`

So inside the server there is no escape hatch: the code path from the axum handler down to the
database **must be async the whole way**. That is the deepest reason the trait has to change.

### 1.6 Why the *CLI* can still use `block_on` (and why that's not a contradiction)

You'll notice `ShiftManager` already calls `self.runtime.block_on(…)` today
(`shift_manager.rs:158` and `:333`) for the Claude summarizer, and Step 3 says to wrap the
now-async `publish` the same way. Doesn't §1.5 forbid that?

No — and the distinction is worth internalizing. The CLI's `main` is a **plain synchronous
function**: `fn main() -> AppResult<()>` (`crates/er-cli/src/main.rs:59`), *not* `#[tokio::main]`.
It builds a runtime by hand: `Arc::new(tokio::runtime::Runtime::new()?)` (`main.rs:65`). The
thread that runs `main` is therefore an **ordinary thread, not a Tokio worker.** Calling
`block_on` from an ordinary thread is exactly what `block_on` is *for*: it's the legitimate
sync→async bridge. The panic in §1.5 only fires when you `block_on` from a thread that is
*already* driving the runtime.

So the rule you're really learning is:

> **`block_on` is correct at the boundary where synchronous code first enters async code, and
> illegal once you are already inside async code.** The CLI is the boundary; the server is already
> inside.

This is why guide §3.1 item 4 says the `ShiftManager` net diff is small: the bridge pattern it
already uses for Claude calls extends naturally to the now-async `publish`.

---

## 2. Foundational concept: async traits and `#[async_trait]`

The guide says to write `#[async_trait::async_trait]` above the trait
(guide §3.1) and explicitly *not* to use "RPITIT." You should understand what that incantation
does and why the alternative is wrong *here specifically*.

### 2.1 The problem: `async fn` in a trait isn't directly usable as `dyn`

Rust lets you write `async fn` directly in a trait definition (this has been stable since Rust
1.75 — it's the feature nicknamed **RPITIT**, "return-position `impl Trait` in traits"). So you
*could* write:

```rust
trait Repository {
    async fn append_event(&self, …) -> Result<u64, StoreError>;
}
```

But there's a catch that collides head-on with how this codebase is built. Look at how the
repository is stored and passed around:

- `ShiftManager` holds `repo: Arc<dyn Repository>` (`shift_manager.rs:43`).
- `EventBus` holds `repository: Option<Arc<dyn Repository>>` (`event_bus.rs:14`).
- The crash-recovery tests use `repo: Arc<dyn Repository>` (`crash_recovery.rs`).

That `dyn Repository` is the important part. **`dyn Trait` is a *trait object*** — it means "some concrete type that implements `Repository`, but I've erased *which* one; I'll dispatch the right method at runtime through a vtable (a table of function pointers)." Trait objects are how this code stays generic over "SQLite today, Postgres tomorrow, a fake in tests" without sprinkling generic type parameters everywhere.

The problem: a trait that uses native `async fn` is **not "dyn-compatible"** (the older term is "object-safe") out of the box. The reason is mechanical. An `async fn` returns an anonymous, compiler-generated `Future` type whose *size is different for every implementation* (SQLite's future and Postgres's future are different state machines of different sizes). A vtable needs a
single, fixed return type to point at. An unsized, per-impl return type can't go in a vtable. So `dyn Repository` with native `async fn` simply doesn't compile in the way you need.

### 2.2 What `#[async_trait]` does about it

The `async-trait` crate (already a dependency of `er-server`, see `crates/er-server/Cargo.toml`)
is a **macro** that rewrites your trait to dodge the problem. When you write:

```rust
#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn append_event(&self, patient_id: PatientId, event_id: Uuid, event: NoteEvent)
        -> Result<u64, StoreError>;
}
```

the macro transforms each `async fn` into a normal `fn` that returns a **heap-allocated, boxed future with a uniform type**:

```rust
fn append_event<'a>(&'a self, …)
    -> Pin<Box<dyn Future<Output = Result<u64, StoreError>> + Send + 'a>>;
```

Now every implementation returns *the same* type — `Pin<Box<dyn Future + Send>>` — so it fits in
a vtable, and `Arc<dyn Repository>` works again. The cost is one heap allocation per call (the
`Box`). For a database call that's about to do network or disk I/O, that allocation is utterly
negligible. That's the trade the guide is making: a trivial allocation in exchange for keeping the
clean `Arc<dyn Repository>` design you already have.

### 2.3 Why *not* native `async fn` / RPITIT here

Native `async fn` in traits is genuinely better *when you don't need `dyn`* — it's zero-cost (no
boxing) and it's what the language now prefers. This codebase already uses it elsewhere: the
`Summarizer` trait is RPITIT, which is *why* `ShiftManager` carries it as a generic parameter
`S: Summarizer` (`shift_manager.rs:31`) instead of `Box<dyn Summarizer>` — the Phase 3 notes in
`CLAUDE.md` call this out explicitly ("generic summarizer, not a trait object").

But `Repository` is the opposite situation: the whole architecture leans on `Arc<dyn Repository>`
so that `EventBus` and `ShiftManager` can hold *one* repo field that might be SQLite *or* Postgres
*or* a remote HTTP client (Step 5's `RemoteRepository`) without becoming generic over the repo
type. Converting all of those to generics would be a much larger, uglier change. So: **`Repository`
keeps `dyn`, therefore `Repository` uses `#[async_trait]`.** Different trait, different trade-off,
for a concrete reason. That asymmetry is a feature of the design, not an inconsistency.

### 2.4 The `Send + Sync` bound, and why it's there

The trait is declared `pub trait Repository: Send + Sync`. Two thread-safety marker traits:

- **`Send`** = "a value of this type can be *moved* to another thread safely."
- **`Sync`** = "a value of this type can be *shared* (by reference) across threads safely."

The server runtime has multiple worker threads, and `Arc<dyn Repository>` gets cloned and handed to tasks that may run on any of them. So the repository must be safe to share across threads —
hence `Sync` — and the futures it returns must be safe to move between threads (Tokio may resume
a paused future on a *different* worker thread than the one that started it) — hence the `+ Send`
on the boxed future in §2.2, which `#[async_trait]` adds for you. This is also why
`SqliteRepository` wraps its connection in `std::sync::Mutex<Connection>` (`repository.rs:34`):
`rusqlite::Connection` is not `Sync` on its own, and the `Mutex` is what makes it shareable. (More
on that mutex in §3.2 — it has a subtle interaction with async.)

---

## 3. The code changes, in dependency order

Now the mechanics. Do them in this order; each builds on the last, and the compiler is your
worklist (fix the errors it points at, top to bottom). The guiding principle from the guide:
**"one trait, N impls" instead of a parallel async hierarchy** — you are *changing* the existing
trait, not adding a second async one.

### 3.1 Change the trait (`crates/er-store/src/repository.rs:8`)

Today:

```rust
pub trait Repository: Send + Sync {
    fn append_event(&self, patient_id: PatientId, event: NoteEvent) -> StoreResult<u64>;
    fn get_events(&self, patient_id: PatientId) -> StoreResult<Vec<StoredEvent>>;
    // …
}
```

After:

```rust
#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn append_event(&self, patient_id: PatientId, event_id: Uuid, event: NoteEvent)
        -> StoreResult<u64>;
    async fn get_events(&self, patient_id: PatientId) -> StoreResult<Vec<StoredEvent>>;
    async fn get_events_since(&self, patient_id: PatientId, sequence_id: u64)
        -> StoreResult<Vec<StoredEvent>>;
    async fn save_snapshot(&self, patient_id: PatientId, sequence_id: u64, snapshot: &PatientNote)
        -> StoreResult<()>;
    async fn get_latest_snapshot(&self, patient_id: PatientId)
        -> StoreResult<Option<(u64, PatientNote)>>;
    async fn event_count(&self) -> StoreResult<usize>;
    async fn snapshot_count(&self) -> StoreResult<usize>;
}
```

Two changes per the guide:
1. `#[async_trait::async_trait]` on the trait, `async` on every method (see §2).
2. **`append_event` gains an `event_id: Uuid` parameter.** This is the identity from §4 below. Add
   `async-trait` and `uuid` to `crates/er-store/Cargo.toml` (uuid is in the workspace deps already;
   just `uuid.workspace = true`; add `async-trait` to the workspace deps table and reference it).

`StoreResult<T>` is just `Result<T, StoreError>` (`crates/er-store/src/error.rs:18`), so the
guide's `Result<T, StoreError>` and the existing `StoreResult<T>` are the same thing — keep using
the alias.

### 3.2 `SqliteRepository`: add `async`, bodies barely change — but understand the mutex rule

The guide says (§3.1 item 1) the SQLite method bodies are "unchanged (no `.await` inside)." That's
true, and *why* it's true is the interesting part.

`SqliteRepository::append_event` (`repository.rs:58`) locks a `std::sync::Mutex`, does a fast
synchronous rusqlite call, and returns. There is no `.await` anywhere in the body. So when you add
`async` to the signature, the body is identical — you're just wrapping a synchronous body in an
async function. An async function with no internal `.await` simply runs straight through; the
`async` is only there to satisfy the trait's new shape.

**But there is a rule you must respect, and it's now compiler-relevant**: *never hold a
`std::sync::Mutex` guard across an `.await` point.* Here's the concept. A `std::sync::Mutex` is a
*blocking* mutex — if a second thread wants the lock, it parks until the first releases it. In
async code, if Task A locks the mutex and then `.await`s (yielding the thread), the thread goes off
and might run Task B, which tries to lock the *same* mutex — and now it blocks a runtime worker
thread, potentially deadlocking, because Task A can't resume to release the lock if all workers are
blocked waiting for it.

You are safe in `SqliteRepository` *precisely because* there is no `.await` between
`self.conn.lock()` and the end of the function — the lock is taken and dropped within one
uninterrupted synchronous stretch. The guide phrases this as: the `std::sync::Mutex` "stays
correct *because* nothing awaits while holding it (the existing invariant, now compiler-adjacent)."
Translation: this was always your rule; now that the function is `async`, the compiler/runtime will
*punish* you if you ever break it. Don't introduce an `.await` between the lock and the unlock.

Also: because rusqlite is genuinely blocking (it touches the disk), a purist would wrap these calls
in `tokio::task::spawn_blocking` so they don't tie up an async worker. For the *device* SQLite path
that's overkill — the calls are microseconds and the CLI isn't a high-concurrency server. The guide
deliberately keeps it simple. Just know that this is a known, accepted simplification, not an
oversight.

### 3.3 `StoredEvent` + the SQLite schema gain `event_id`

`StoredEvent` (`repository.rs:254`) is the struct returned when you read events back:

```rust
pub struct StoredEvent {
    pub sequence_id: u64,
    pub event: NoteEvent,
    pub occurred_at: DateTime<Utc>,
}
```

Add `pub event_id: Uuid`. Then the two `get_events*` methods that build `StoredEvent`s
(`repository.rs:91` and `:142`) must `SELECT` the new column and populate the field.

The schema in `crates/er-store/src/db.rs:13` gains the column:

```sql
CREATE TABLE IF NOT EXISTS events (
    sequence_id  INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id     TEXT NOT NULL UNIQUE,     -- NEW: client-stamped UUIDv7
    patient_id   TEXT NOT NULL,
    event_type   TEXT NOT NULL,
    payload      TEXT NOT NULL,
    occurred_at  TEXT NOT NULL
);
```

Because there is no production data yet (it's a dev DB you delete freely), you just edit the
`CREATE TABLE` — no migration tooling needed on the SQLite side. Then `append_event`'s INSERT lists
`event_id`, and — per the guide's idempotency rule — uses **`INSERT OR IGNORE`**: if a row with that
`event_id` already exists (the `UNIQUE` constraint fires), the insert is silently skipped instead of
erroring. After an `INSERT OR IGNORE`, you must detect whether you inserted or ignored:
`conn.changes()` returns 0 on ignore; in that case `SELECT sequence_id FROM events WHERE event_id = ?`
to return the *existing* row's sequence. That way a duplicate append returns the same `sequence_id`
as the original — idempotency on the device, mirroring what Postgres will do in §5. This is the
SQLite half of "idempotency on-device too" (guide §3.1 item 5).

### 3.4 `EventBus::publish` becomes `async` — and is where `event_id` is *minted*

`EventBus::publish` (`event_bus.rs:27`) is the chokepoint every write goes through. It currently:

1. extracts the patient id,
2. calls `repo.append_event(patient_id, event.clone())?` (line 33),
3. synchronously projects into the in-memory `DashMap`,
4. conditionally saves a snapshot,
5. broadcasts.

Changes:

- Signature becomes `pub async fn publish(&self, event: NoteEvent, store: &PatientNoteStore) -> AppResult<u64>`.
- The repo calls become awaited: `repo.append_event(patient_id, event_id, event.clone()).await?` and
  `repo.save_snapshot(...).await`.
- **This is where the `event_id` is born.** For events that *originate on this device* (the CLI
  user adds a lab, stops an encounter, etc.), `publish` stamps a fresh id:
  `let event_id = Uuid::now_v7();` and passes it to `append_event`. (Step 5's sync API will instead
  carry an `event_id` that the *client* already stamped — see §4.3 on why the *client*, not the
  server, owns the id. For now, on-device, the device *is* the client, so `publish` minting it is
  correct.)

**The DashMap-across-await hazard (the async twin of §3.2's mutex rule).** Look carefully at
`publish` lines 41–48 and 63. The code is *already written* to clone data out of the `DashMap`
*before* doing anything else, and the comments say so ("DashMap write lock on this shard is
dropped"; "Clones the PatientNote out of the ref immediately, which then drops the read lock before
`save_snapshot` is called"). This was good hygiene before; now it's **mandatory**. A `DashMap`
shard guard is a lock, exactly like the mutex in §3.2. If you held a `dashmap` `Ref`/`RefMut`
across the `.await` on `save_snapshot`, you'd risk the same deadlock class. The existing structure —
compute `phase_after` inside a block that ends (dropping the guard), then clone the note out with
`store.get(&patient_id).map(|r| r.clone())` *before* the awaited `save_snapshot` — is precisely the
shape that keeps you safe. When you sprinkle `.await` into this function, **do not move any `.await`
inside a block that's still holding a DashMap guard.** Keep every `.await` in the "no lock held"
regions. The guide compresses all of this into one sentence (§3.1 item 2): "clone out of the map
before any `.await`; never hold a DashMap guard across an await point." Now you know what disaster
that sentence is preventing.

### 3.5 `load_patient_note` becomes `async` (`crates/er-app/src/load.rs:27`)

This function calls `repo.get_latest_snapshot`, `repo.get_events_since`, and `repo.get_events` —
all now async. So it becomes `pub async fn load_patient_note(...)` and `.await`s each repo call. Its
internal logic (snapshot fast-path, then delta replay; else full replay) is unchanged — only the
calls grow `.await`. `apply_event` and `replay` are pure domain functions with no I/O
(`er-domain`), so they stay synchronous and are called normally.

### 3.6 `ShiftManager`: wrap the now-async calls with `block_on` (two *kinds* of site)

`ShiftManager` is the sync→async boundary for the CLI (§1.6). It already owns `runtime: Arc<Runtime>`
(`shift_manager.rs:45`). There are **two categories** of call site to fix:

**(a) Calls that go through `EventBus::publish`** — there are many: `add_lab` (`:219`),
`set_disposition` (`:226`), `record_medication` (`:254`), `assign_diagnosis` (`:277`),
`record_data_reviewed` (`:286`), `trigger_mdm` (`:339`, `:342`), `review_ready` (`:354`),
`sign` (`:365`), and the summarize path (`:163`). Each `self.bus.publish(ev, &self.store)?` becomes
`self.runtime.block_on(self.bus.publish(ev, &self.store))?`. This is the *same* pattern already used
two lines away for the summarizer (`self.runtime.block_on(self.summarizer.summarize_encounter(...))`
at `:158`) — you're applying a familiar move to a new set of calls.

**(b) Calls that hit the repo *directly*** — easy to miss. `reload_active_patient`
(`shift_manager.rs:440`) calls `self.repo.get_latest_snapshot(...)`, `self.repo.get_events_since(...)`,
and `self.repo.get_events(...)` *without* going through the bus. Those are now async too, so each
needs `block_on`. Likewise `event_count` (`:428`) and `snapshot_count` (`:432`) call
`self.repo.event_count()?` / `snapshot_count()?` directly — wrap those as well. **Search for every
`self.repo.` and every `self.bus.publish` in this file; that set is your exact edit list.**

One nuance worth thinking about: each `block_on` runs *one* publish to completion before the next
line. That's fine and correct for the CLI's one-thing-at-a-time model. You are not trying to make
the CLI concurrent — you're just satisfying the new async signatures at the boundary.

### 3.7 Add the missing `clinical_occurred_at` accessor on `NoteEvent` (a real gap)

The guide's `PostgresRepository` sketch (§3.2) inserts `event.get_clinical_event_occurred_at()` into
the `clinical_occurred_at` column. **That method does not exist yet.** Every `NoteEvent` variant
*carries* a `clinical_occurred_at: DateTime<Utc>` field (`crates/er-domain/src/events.rs:11`
onward), but there is no accessor — the same way `extract_patient_id` (`events.rs:97`) and
`event_type` (`events.rs:115`) are hand-written `match` arms over every variant, you need a third:

```rust
pub fn clinical_occurred_at(&self) -> DateTime<Utc> {
    match self {
        NoteEvent::AudioStarted { clinical_occurred_at, .. } => *clinical_occurred_at,
        // … one arm per variant; every variant has this field …
    }
}
```

(Name it whatever you like; the guide called it `get_clinical_event_occurred_at` — match that or
rename the guide. Consistency with the existing `extract_*`/`event_type` naming would argue for
`clinical_occurred_at()`.) Add it in `er-domain`, since it's pure domain logic with no I/O.

**Why this matters conceptually — the two-timestamps idea.** Right now SQLite's `append_event`
stores `occurred_at = Utc::now().to_rfc3339()` (`repository.rs:63`). That is the *recorded* time —
"when the database wrote this row." But the event also knows its *clinical* time —
`clinical_occurred_at` — "when the thing actually happened to the patient." These can differ: an
event captured offline in an ED dead-zone and synced an hour later has a clinical time of an hour
ago but a recorded time of now. The Postgres schema (`phase4.md:233`) has **both columns** on
purpose: `clinical_occurred_at TIMESTAMPTZ NOT NULL` (from the event) and
`recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()` (the DB clock). Replay determinism depends on the
*clinical* time (per `CLAUDE.md`: "All carry `clinical_occurred_at`, which `apply_event` uses for
`updated_at`/audit/section-version timestamps so replay is deterministic"). So as you port, decide
deliberately what `StoredEvent.occurred_at` means and consider storing the clinical time for replay
fidelity. This is a place where the SQLite implementation is currently a little loose and the
Postgres port is your chance to make the clinical-vs-recorded distinction explicit.

### 3.8 Tests: `#[test]` → `#[tokio::test]`

Any test that now touches an async repo/bus/load call must run *inside* a runtime. `#[tokio::test]`
(instead of `#[test]`) spins up a small runtime for that one test and lets the test body be `async`,
so you can `.await` directly. Concretely, in `crates/er-app/tests/persistence_integration.rs` every
`bus.publish(...).unwrap()` becomes `bus.publish(...).await.unwrap()`, every
`load_patient_note(...)` becomes `...await`, and the `#[test]` attributes become `#[tokio::test]`.
Same surgery on `crash_recovery.rs`. The *assertions* don't change — only the plumbing that gets you
the values does. (And `append_event` calls inside tests must pass an `event_id` now —
`Uuid::now_v7()` per call, or whatever the test wants to assert idempotency with.)

---

## 4. Foundational concept: event identity & idempotency (`event_id`)

This is the *protocol* idea behind Step 3, and it's the part most worth slowing down on because it's
about distributed-systems correctness, not Rust syntax.

### 4.1 What "idempotent" means and why you need it

An operation is **idempotent** if doing it twice has the same effect as doing it once. `append_event`
*without* an identity is **not** idempotent: call it twice with "the same" event and you get two rows
with two different `sequence_id`s — the note now thinks two labs were ordered, or two encounters
happened. That's data corruption.

Why would anyone call it twice? Because **networks are unreliable**, and the sync protocol (Step 5)
has to cope. Picture a phone uploading a batch of events to the server:

1. Phone sends the batch.
2. Server receives it, writes the events, sends back "OK."
3. **The "OK" is lost** — the phone's connection drops in an ED dead-zone before the ack arrives.
4. The phone, having no ack, reasonably assumes the upload failed and **retries the same batch.**

Without idempotency, step 4 duplicates every event. This is not an edge case; on flaky hospital Wi-Fi
it's routine. The standard term for this delivery model is **"at-least-once"**: the network can
guarantee a message arrives *at least* once, but not *exactly* once — duplicates are inevitable. The
job of `event_id` is to convert at-least-once *delivery* into **exactly-once *storage*.**

### 4.2 How `event_id` achieves exactly-once storage

Give each event a globally unique id *at creation time*. Make the `event_id` column `UNIQUE` in the
database. Then "insert this event" becomes "insert this event *unless one with this id already
exists*":

- Postgres: `INSERT … ON CONFLICT (event_id) DO NOTHING`.
- SQLite: `INSERT OR IGNORE …`.

Now the retry in §4.1 step 4 is harmless: the second insert sees the `event_id` already present and
does nothing. The note has exactly one of each event regardless of how many times the batch was
uploaded. The id is the dedup key.

### 4.3 Why the *client* stamps the id, not the server

This is the subtle, important decision (plan decision D10). The id must be assigned **by whoever
creates the event** (the device), *before* the first upload attempt — **not** generated by the
server on receipt.

Reason: the whole point is to recognize a *retry of the same event*. If the server generated the id,
then the original upload and the retry would arrive as two requests with no id yet, and the server
would mint *two different* ids — it could never tell they were the same event. By having the device
stamp the id once and reuse it on every retry, *all* retries of that event carry the *same* id, and
the `UNIQUE` constraint can recognize them as duplicates. The identity has to be born at the source
and travel with the event.

This is why `sequence_id` can't do this job. `sequence_id` is assigned *by the database* on insert
(`BIGSERIAL`/`AUTOINCREMENT`) — it's the *server's* ordering, decided *after* arrival. It's perfect
for "what order did the server learn about these events" (which is what `?since=` paging in Step 5
walks), but useless for dedup, because a duplicate upload would get a *new* `sequence_id`. You need
**two** identifiers with two different jobs: `event_id` (client-stamped identity, for dedup) and
`sequence_id` (server-assigned order, for paging). Step 3 introduces the first; the second already
exists.

### 4.4 Why UUIDv7 specifically

A UUID is a 128-bit globally-unique id with effectively zero collision probability, so two different
devices generating ids independently will never clash — no coordination needed. The codebase already
uses `Uuid::now_v7()` for patient and encounter ids (e.g. `events.rs:283`), and the workspace enables
the `v7` feature (`Cargo.toml:16`).

The "**v7**" part matters. The original random UUID (v4) is pure randomness. **UUIDv7 embeds a
millisecond timestamp in its high bits**, so v7 ids are **time-ordered**: an id generated later sorts
*after* one generated earlier. Two payoffs:

- It's a sensible, monotonic-ish identity that roughly tracks creation time for free.
- Database B-tree indexes on the `event_id` column stay compact: inserts land at the "end" of the
  index rather than scattering randomly across it (random v4 ids fragment the index and hurt write
  performance). For a write-heavy event store this is a real, measurable win.

So: globally unique (no coordination) + time-ordered (index-friendly) = UUIDv7 is the right tool, and
the guide's `Uuid::now_v7()` is exactly that.

### 4.5 Why the id lives in the *envelope*, not in every `NoteEvent` variant

A tempting wrong turn (guide §3.1 item 5 warns against it explicitly): "add an `event_id` field to
every `NoteEvent` variant." Don't. There are 13 variants (`events.rs:11`), each would need the field,
every constructor and every `match` arm would grow, and — worse — the id would become part of the
*domain* event's identity, entangling a pure clinical concept ("a lab was received") with a
*transport/storage* concept ("the unique key we dedup on").

Instead, the id rides in the **envelope** — the metadata *around* the event, not *inside* it. The
"envelope" here is the `(event_id, patient_id, sequence_id, occurred_at, payload)` row in the
database and the `StoredEvent` struct that mirrors it. So:

- `EventBus::publish` mints the `event_id` and passes it *alongside* the `NoteEvent` to
  `append_event(patient_id, event_id, event)` — the id is a sibling parameter, not a field of `event`.
- `StoredEvent` (the read side) gains `event_id` as a struct field (§3.3) — again, next to the event,
  not inside it.
- The `NoteEvent` enum stays **completely untouched**. Pure domain stays pure; identity is a
  storage-layer concern handled at the storage boundary.

This is the same separation-of-concerns discipline the codebase already follows — domain knows
nothing about persistence. Keep it that way.

---

## 5. Foundational concept: `sqlx` and `PostgresRepository`

With the trait async and identity-aware, the new implementation is the reward. Concepts first, then
how they map to the guide's sketch.

### 5.1 What `sqlx` is, and "compile-time-checked queries"

`sqlx` is an async, pure-Rust SQL toolkit. Its headline feature: the `sqlx::query!` macro **checks
your SQL against a real database schema at compile time.** When you write
`sqlx::query!("INSERT INTO events (…) VALUES ($1,…) RETURNING sequence_id", …)`, the macro (during
`cargo build`) connects to a database (or reads a cached schema, see §5.2), asks "is this SQL valid?
what types do the `$1…` parameters need? what columns/types come back?", and then **generates a
typed Rust struct for the result.** If you typo a column name, pass a `String` where the column is a
`UUID`, or read a column that doesn't exist, **your code doesn't compile.** This catches an entire
category of bug (malformed SQL, type mismatches) at build time instead of in production at 2 a.m.
It's the Rust-flavored reason to prefer `sqlx` over a stringly-typed driver.

### 5.2 The `.sqlx` offline cache and why you commit it

There's a chicken-and-egg wrinkle: if `query!` needs a live database to check against, then CI (and
teammates) would need a running Postgres just to *compile*. `sqlx` solves this with **offline mode**.
You run `cargo sqlx prepare --workspace` once (against your local compose Postgres); it writes the
query metadata into a `.sqlx/` directory. You **commit `.sqlx/`**. Then, with `SQLX_OFFLINE=true`
(CI sets this), the macro reads the cached metadata instead of connecting — so builds need no
database. The rule to internalize: **every time you add or change a `query!`, re-run
`cargo sqlx prepare` and commit the updated `.sqlx/`**, or CI builds will fail with "query not found
in cache." The guide's "Run `cargo sqlx prepare --workspace` and commit `.sqlx/`" (§3.2) is this.

### 5.3 Migrations

A **migration** is a versioned `.sql` file describing a schema change. `sqlx migrate add <name>`
creates a timestamped file under `crates/er-server/migrations/`; you write the DDL (the `CREATE
TABLE`s) into it; `sqlx migrate run` applies any not-yet-applied migrations to the database, tracking
which ran in a `_sqlx_migrations` table. Migration `0001` holds the `events` + `snapshots` DDL from
`phase4.md:233` (plus the tenancy tables `orgs`/`clinicians`, ordered so foreign keys resolve — the
guide gives you two valid orderings; pick one). Later steps add migrations `0003`–`0005`. This is
how the cloud schema evolves safely over time, in contrast to the SQLite side where you just edit the
`CREATE TABLE` because there's no data to preserve.

### 5.4 `PgPool` — connection pooling

Opening a Postgres connection is expensive (TCP + TLS + auth handshake). You don't want to do that
per request. A **`PgPool`** (`sqlx::PgPool`) keeps a set of open connections alive and hands one to
each query, returning it to the pool afterward. It's created once at server boot
(`PgPool::connect(&cfg.database_url).await?`) and lives in `AppState` (`crates/er-server/src/state.rs`
— note the TODO comment there already names this as "the canonical reason `build` is async+fallible").
Importantly, `PgPool` is internally reference-counted, so cloning `AppState` per request (axum does
this) is cheap — a refcount bump, not a new connection. That's the "`AppState` must stay cheaply
`Clone`" note in `state.rs`.

### 5.5 The org-scoped constructor — tenancy that can't be forgotten (D9)

```rust
pub struct PostgresRepository { pool: sqlx::PgPool, org_id: Uuid }
impl PostgresRepository {
    pub fn for_org(pool: PgPool, org_id: Uuid) -> Self { … }
}
```

This is a security-architecture decision, not a mere convenience. The product is **multi-tenant**:
many hospitals/orgs share one database. The cardinal rule (plan D9) is *org A must never see org B's
data.* The design enforces this by **baking the `org_id` into the repository itself**: a
`PostgresRepository` is constructed *for one org*, and *every* query it runs includes
`WHERE org_id = self.org_id`. Because there's no way to get a repository that *isn't* org-scoped,
there's no code path that can accidentally query across tenants. Compare the alternative — passing
`org_id` into each handler and trusting every handler to add the filter — where one forgotten
`WHERE` clause is a cross-tenant data leak. The constructor approach makes the safe thing the *only*
thing. Step 4 builds the auth middleware that turns a JWT into the right `org_id` and constructs the
per-request `PostgresRepository::for_org(pool, authed.org_id)`; Step 3 just sets up the shape.

### 5.6 The `append_event` body: `ON CONFLICT … DO NOTHING` + `RETURNING` + the duplicate lookup

Walk the guide's sketch (§3.2) line by line:

```rust
let rec = sqlx::query!(
    r#"INSERT INTO events (event_id, org_id, patient_id, event_type, payload, clinical_occurred_at)
       VALUES ($1,$2,$3,$4,$5,$6)
       ON CONFLICT (event_id) DO NOTHING
       RETURNING sequence_id"#,
    event_id, self.org_id, patient_id, event.event_type(),
    serde_json::to_value(&event)?, event.clinical_occurred_at(),   // ← the accessor from §3.7
).fetch_optional(&self.pool).await?;
match rec {
    Some(r) => Ok(r.sequence_id as u64),
    None    => self.sequence_for(event_id).await,   // duplicate ⇒ look up existing seq
}
```

- **`ON CONFLICT (event_id) DO NOTHING`** — the Postgres realization of §4.2. If a row with this
  `event_id` already exists (a retry), don't insert, don't error — do nothing.
- **`RETURNING sequence_id`** — Postgres can hand back columns from the row it just inserted. You want
  the server-assigned `sequence_id` so the caller (and ultimately the syncing client) learns where
  this event landed in the order.
- **`fetch_optional`** — returns `Option<Record>`. Here's the clever bit: on a *successful insert*,
  `RETURNING` yields a row → `Some`. On a *conflict* (DO NOTHING), nothing was inserted, so
  `RETURNING` yields *no* row → `None`. So `Some` vs `None` *is* your "did I insert or was it a
  duplicate?" signal.
- **The `None` branch** — a duplicate still needs to return a *stable* `sequence_id` (the caller asked
  "where did this event land?" and deserves the same answer every time). So you `SELECT sequence_id
  FROM events WHERE event_id = $1` to fetch the *original* row's sequence. Write that as a small
  `sequence_for(event_id)` helper. This is the exact mirror of the SQLite `INSERT OR IGNORE` →
  `conn.changes() == 0` → re-`SELECT` logic from §3.3. Both implementations now behave identically
  under duplicate appends — which is what makes the "run the same suite against both" test in §6
  meaningful.
- `serde_json::to_value(&event)?` serializes the `NoteEvent` to JSON for the `JSONB` column. This is
  *the same wire format* the SQLite `payload TEXT` already stores (`repository.rs:60` does
  `serde_json::to_string`). `JSONB` is Postgres's binary-JSON column type — queryable and indexable,
  unlike a plain text blob. Same bytes conceptually, richer storage.

`get_events`/`get_events_since` are `SELECT … WHERE org_id = $1 AND patient_id = $2 ORDER BY
sequence_id` — note the `org_id` filter (§5.5) that the SQLite version doesn't have, because SQLite
is single-tenant. `save_snapshot`/`get_latest_snapshot` reuse the **same SHA-256 hex checksum logic**
the SQLite impl already has (`repository.rs:166` and `:215`) and the same `note.hydrate()` call on
load (`repository.rs:232`) — the snapshot integrity design carries over unchanged; only the storage
engine differs.

### 5.7 Where `PostgresRepository` lives, and the dependency additions

Per guide §1.1's note, `PostgresRepository` goes in **`er-server`**, *not* `er-store`. The reason is
the dependency graph: if the Postgres impl lived in `er-store`, then the *device* build (CLI, future
iOS) would compile `sqlx` even though it only ever uses SQLite. Putting it in `er-server` keeps
`sqlx` out of the device's dependency tree entirely. So `er-server` needs these added to its
`Cargo.toml`:

- `sqlx` with the features in guide §3.2 (`postgres,runtime-tokio,tls-rustls,uuid,chrono,json,migrate`),
- `er-store` as a **direct** dependency (it currently only depends on `er-app`) so it can name the
  `Repository` trait, `StoreError`, and `StoredEvent` to implement them,
- `er-domain` as a direct dependency for `PatientNote`, `PatientId`, `NoteEvent`,
- `uuid` (already present).

(`async-trait` is already in `er-server/Cargo.toml` — the author anticipated this step.)

---

## 6. Foundational concept: one test suite, two implementations

### 6.1 Why test against both

You now have two implementations of the *same contract* (`Repository`): SQLite and Postgres. The
whole value of the trait is that they're interchangeable. The way you *prove* they're
interchangeable — and that the Postgres port didn't subtly break replay, snapshots, or checksums — is
to run **the same assertions against both.** If a test passes on SQLite but fails on Postgres, you've
found a behavioral divergence; if it passes on both, the abstraction holds.

### 6.2 How: assertions over `Arc<dyn Repository>`

Refactor the test bodies so the logic lives in a function that takes the trait object:

```rust
async fn suite(repo: Arc<dyn Repository>) {
    // append events, read them back, save/load a snapshot, corrupt a checksum,
    // append the same event_id twice and assert one row + identical sequence_id, …
}
```

Then two thin entry points call it: one constructs a `SqliteRepository` (in-memory, as the tests do
today — `open_memory_db()`), the other a `PostgresRepository`. Because both are
`Arc<dyn Repository>`, `suite` doesn't know or care which it got — that's §2's trait object earning
its keep. Note the *EventBus/load* tests (which live in `er-app` and test projection + snapshot
*orchestration*) are a layer above the raw trait; the pure repository contract tests (append/read/
snapshot/checksum/idempotency) are what go in the shared `suite`. Keep the distinction clear: `suite`
tests the storage contract; the existing `persistence_integration.rs` tests the application's use of
it.

### 6.3 `#[sqlx::test]` — an isolated database per test

For the Postgres entry point, the `#[sqlx::test(migrations = "./migrations")]` macro does something
very convenient: before each test it **creates a brand-new, empty database, runs your migrations into
it, and hands your test a `PgPool` for it** — then drops it afterward. So every test gets a pristine,
isolated schema with zero shared state between tests, against your compose Postgres
(`DATABASE_URL`). This is why the guide can casually say "run the crash-recovery suite against
Postgres": the macro makes spinning up a clean DB per test a one-liner. And the crash-recovery suite
passing on Postgres is the real proof that the *event-sourcing semantics* — replay from events,
snapshot fast-path, checksum fallback, mid-encounter reload — survived the port intact. That suite is
the conscience of this step.

---

## 7. The gate (§3.4), explained — what each line *proves*

```
$ cargo test --workspace
```
The existing SQLite-backed tests, now async, still pass. **Proves: the async conversion didn't break
on-device behavior.** This is your regression net — if Phase 2/3 functionality survived the refactor,
these stay green.

```
$ docker compose up -d postgres && sqlx migrate run --source crates/er-server/migrations
```
Brings up the local Postgres and applies your migrations. **Proves: your DDL is valid and applies
cleanly from scratch.**

```
$ DATABASE_URL=… cargo test -p er-server
```
Runs the generic suite + crash-recovery against real Postgres. **Proves: the Postgres impl satisfies
the same contract SQLite does, including event-sourcing semantics.**

```
# Idempotency proof: append the same (event_id, event) twice → event_count == 1,
# both calls return the same sequence_id.
```
Write this test explicitly. Append once, capture the returned `sequence_id`. Append the *same
`event_id`* again. Assert (a) `event_count()` is still 1 (the `ON CONFLICT`/`OR IGNORE` worked), and
(b) the second call returned the *same* `sequence_id` as the first (the duplicate-lookup branch in
§5.6 / §3.3 worked). **Proves: at-least-once delivery → exactly-once storage** — the entire point of
§4, demonstrated. This single test is the headline deliverable of the `event_id` work; everything in
§4 exists to make it pass.

---

## 8. Suggested order of attack (so the compiler guides you)

1. **`er-domain`:** add the `clinical_occurred_at()` accessor on `NoteEvent` (§3.7). Smallest, purest
   change; nothing depends on it failing.
2. **`er-store` trait + SQLite (§3.1–3.3):** add `#[async_trait]`, `async`, the `event_id` param,
   the `StoredEvent` field, the schema column, and `INSERT OR IGNORE` + duplicate lookup. Build
   `er-store` alone (`cargo build -p er-store`) until green.
3. **`er-app` (§3.4–3.5):** make `EventBus::publish` and `load_patient_note` async; mint `event_id`
   in `publish`; respect the no-await-while-locked rule. Build `er-app`.
4. **`er-app` `ShiftManager` (§3.6):** wrap every `self.bus.publish` and every `self.repo.` call in
   `block_on`. Build the CLI (`cargo build -p er-cli`).
5. **Tests (§3.8):** convert to `#[tokio::test]`, add `.await`s, thread `event_id`s. Get
   `cargo test --workspace` green — the whole SQLite world now works async.
6. **`er-server` (§5):** add deps, write migrations, `PostgresRepository`, `cargo sqlx prepare`,
   commit `.sqlx/`.
7. **Generic suite + Postgres tests (§6):** refactor to `suite(Arc<dyn Repository>)`, add the
   `#[sqlx::test]` Postgres entry point and the idempotency test. Run the §7 gate.

Commit when the gate is fully green — not before. This step has no half-done state worth committing;
the async conversion is all-or-nothing across the workspace.

---

## 9. Pitfalls specific to *this* codebase

- **The DashMap guard in `publish` (`event_bus.rs:41–63`).** The single most likely place to
  introduce a deadlock when you add `.await`. Keep every `.await` outside any block holding a
  `DashMap` `Ref`/`RefMut`. The existing clone-then-drop structure is correct — preserve it (§3.4).
- **`reload_active_patient`, `event_count`, `snapshot_count` call the repo directly** — not through
  the bus. Easy to miss when you're grepping for `publish`. Grep for `self.repo.` too (§3.6b).
- **The `clinical_occurred_at` accessor genuinely doesn't exist yet** — the guide assumes it. Don't
  burn time hunting for it; add it (§3.7).
- **`event_count`/`snapshot_count` are unscoped global counts** (`repository.rs:237`, `:245`). On
  Postgres with multi-tenancy they'd count across orgs, which is meaningless in production. The guide
  keeps them as "test-support." Fine for now — just don't wire them into any real handler; treat them
  as test instrumentation only.
- **Forgetting `cargo sqlx prepare` after editing a query** → green locally (you have a DB), red in CI
  (it doesn't). Make "prepare + commit `.sqlx/`" muscle memory (§5.2).
- **The clinical-vs-recorded timestamp decision (§3.7).** The SQLite path currently stamps
  `Utc::now()` as `occurred_at`. The Postgres schema has both `clinical_occurred_at` and
  `recorded_at`. Decide deliberately which one `StoredEvent.occurred_at` represents and store the
  clinical time where replay needs it — don't paper over the distinction.

---

*Once §7's gate is green, the trait is async, identity-bearing, and proven against two backends — and
Steps 4–7 (auth, sync API, audio worker, server AI) all have a real async persistence layer to build
on. That is the entire purpose of Step 3.*
