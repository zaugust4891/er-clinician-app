# State Pattern Implementation Guide

Reference: The Rust Programming Language, Chapter 17 — "Implementing an Object-Oriented Design Pattern"

---

## Overview

Replace the current **enum + match** state machine with **trait objects where each state is a struct**. Currently the phase enum lives in `src/models/note.rs` and the transition logic lives in `src/state.rs` as a free function with a big match. The state pattern distributes behavior into each state struct instead.

---

## Files to Modify

### 1. `src/state.rs` — Primary Rewrite

This is where the state pattern lives. Replace the free `transition()` function and its match block with:

- A `NoteState` trait defining what behavior each state exposes
- One struct per state (`Registered`, `Encountering`, `Idle`, `Finalizing`, `PendingSignOff`, `Signed`)
- Each struct implements `NoteState`, and its methods return `Box<dyn NoteState>` for transitions

```rust
// The trait — each state implements this
trait NoteState {
    /// Handle an audio-start event. Default: invalid transition (returns self).
    fn on_audio_started(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        self  // default: no transition, stay in current state
    }

    fn on_audio_stopped(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        self
    }

    fn on_trigger_mdm(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        self
    }

    fn on_review_ready(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        self
    }

    fn on_sign(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        self
    }

    /// Can results be ingested in this state?
    fn can_ingest_results(&self) -> bool {
        true  // all states except Signed
    }

    /// Name for serialization and display.
    fn phase_name(&self) -> &'static str;
}
```

Then each state struct implements only the transitions valid **for that state**:

```rust
struct Idle;

impl NoteState for Idle {
    fn on_audio_started(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        note.encounter_count += 1;
        Box::new(Encountering)
    }

    fn on_trigger_mdm(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        Box::new(Finalizing)
    }

    fn phase_name(&self) -> &'static str { "Idle" }
}

struct Signed;

impl NoteState for Signed {
    fn can_ingest_results(&self) -> bool { false }  // override the default
    fn phase_name(&self) -> &'static str { "Signed" }
    // All transition methods use the default (return self) — nothing is valid from Signed
}
```

Invalid transitions don't need explicit error handling in a match arm. They simply don't exist as methods on that state. The default trait method returns `self` (no-op), or you can make it return a `Result` if you want to surface errors.

The `PatientNoteStore` type alias and `new_store()` function stay in this file unchanged.

---

### 2. `src/models/note.rs` — Change How `PatientNote` Holds State

Currently:
```rust
pub phase: NotePhase,   // enum variant
```

With the state pattern:
```rust
pub state: Option<Box<dyn NoteState>>,  // trait object
```

The `Option` wrapper is important — it lets you **take** the state out of the note, call a method on it (which consumes `self: Box<Self>`), and put the new state back:

```rust
impl PatientNote {
    pub fn handle_audio_started(&mut self) {
        if let Some(state) = self.state.take() {
            self.state = Some(state.on_audio_started(self));
        }
    }
}
```

This is exactly the pattern from the book — `take()` moves the state out, the method consumes it and returns the next state, and you put the result back.

#### Derived Trait Problems and Solutions

`PatientNote` derives `Serialize, Deserialize, Clone, Debug`. Trait objects (`Box<dyn NoteState>`) don't implement any of those. Solutions:

**Serialization:** Keep the `NotePhase` enum alongside the trait object. Use `phase_name()` to serialize, and reconstruct the trait object from the enum when deserializing. The enum becomes a serialization-only representation.

**Clone:** Implement manually, or add a `fn clone_state(&self) -> Box<dyn NoteState>` method to the trait.

**Debug:** Add a `fn debug_name(&self) -> &str` to the trait, implement `Debug` manually on `PatientNote`.

---

### 3. `src/error.rs` — Minor Update

The `InvalidTransition` variant currently takes `from: NotePhase`. If you keep the enum for error reporting, no change needed. If you drop the enum entirely, change it to `from: String` and use `phase_name()`.

---

## Files That Don't Change

- **`src/events.rs`** — `NoteEvent` stays exactly as-is. Events are *inputs* to the state machine, not states. The state pattern changes how states handle events, not how events are defined.
- **`src/models/encounter.rs`, `result.rs`, `disposition.rs`, etc.** — Untouched. These are data types, not behavior.
- **`src/audio.rs`, `stt.rs`, `summarizer.rs`** — Untouched. These are separate subsystems that produce events.
- **`src/export.rs`** — Reads from `NoteSections`, doesn't care about state.

---

## Files to Update Lightly

### `src/db.rs` / `src/repository.rs`

When saving/loading a `PatientNote`, serialize the state as a string and reconstruct the trait object on load:

```rust
fn state_from_name(name: &str) -> Box<dyn NoteState> {
    match name {
        "Registered" => Box::new(Registered),
        "Encountering" => Box::new(Encountering),
        "Idle" => Box::new(Idle),
        "Finalizing" => Box::new(Finalizing),
        "PendingSignOff" => Box::new(PendingSignOff),
        "Signed" => Box::new(Signed),
        _ => panic!("unknown state: {name}"),
    }
}
```

### `src/models/mod.rs`

If you export `NoteState` as a public trait, add it to re-exports.

### `src/main.rs`

Update how you interact with the note — call methods instead of the free `transition()` function.

---

## Trade-off Comparison

| | Current (enum + match) | State Pattern (trait objects) |
|---|---|---|
| **Invalid transitions** | Runtime error from catch-all `_ => false` | Default no-op (silent) or explicit per-method |
| **Adding a new state** | Add enum variant, update every match arm (compiler forces you) | Add a new struct + impl. Existing states untouched. |
| **Adding a new event** | Add match arms in one place | Add a new method to the trait + every impl (compiler forces you) |
| **Serialization** | Free via `#[derive(Serialize)]` on enum | Requires manual bridge between trait object and string/enum |
| **State-specific behavior** | Must check phase everywhere: `if note.phase == Signed { ... }` | Move the behavior into the state: `state.can_ingest_results()` |
| **Complexity** | ~85 lines, one function | ~200 lines across 6 structs + trait |

---

## When the State Pattern Pays Off

The state pattern shines when states have different *behavior*, not just different *transitions*. Right now the states are mostly about "which transitions are allowed." But as Phase 2 and 3 are built out, states will develop real behavioral differences:

- `Encountering` buffers audio and triggers STT
- `Idle` accepts results and queues summarization
- `Finalizing` triggers LLM calls for MDM generation
- `Signed` rejects all mutations

That's when the pattern pays off — behavior lives inside the state instead of being guarded by `if phase == X` checks scattered across the codebase.

---

## Recommendation

If implementing now, **keep the `NotePhase` enum too** — use it for serialization, display, and error messages. Use the trait objects for behavior. The enum is the "data" representation, the trait objects are the "behavior" representation. They stay in sync via `phase_name()`.
