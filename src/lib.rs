//! Layered architecture (see docs/planning/folder-structure-options.md, Option 1):
//!
//! - `domain` — pure core: aggregate, state machine, events, replay.
//!   Imports nothing from the other layers and performs no I/O.
//! - `application` — use-case orchestration: EventBus, ShiftManager,
//!   EncounterDriver, note loading, CLI conversions.
//! - `infrastructure` — the outside world: SQLite persistence, Claude AI,
//!   audio capture, speech-to-text.
//! - `interface` — delivery: CLI command handling, note export.
//!
//! Dependency rule: arrows point inward only (interface/infrastructure ->
//! application -> domain). `error` is shared by all layers.

pub mod application;
pub mod domain;
pub mod error;
pub mod infrastructure;
pub mod interface;
