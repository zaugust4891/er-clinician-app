# Doctor Feedback → Gap Analysis

**Date:** 2026-06-08
**Author:** Engineering (gap analysis pass)
**Scope:** Reconcile Dr. Joey's feedback (both rounds) against the *actual* codebase and the existing roadmap docs, then produce a buildable plan for the genuine gaps.

**Non-negotiables honored throughout:** the re-evaluation flow (multi-encounter merging) and patient-switching functionality **stay**. Nothing below proposes removing or deprecating any existing feature. Where Dr. Joey is lukewarm on re-eval, the response is to *reduce the product's dependence* on it, not to remove it.

---

> ## ⏩ Phase 3 status update (2026-06-14)
>
> **Phase 3 (`docs/planning/phase3.md`) is complete (Steps 0–9) and closes the central finding in §1.** The AI generation layer is now wired into the live event-sourced pipeline:
> - **F3 / F8 / F9 → ✅ built (backend):** `stop_encounter` auto-summarizes HPI/Exam; `trigger_mdm` is a real one-button MDM generator that assembles results + meds + diagnoses + disposition, calls `generate_mdm`, persists `MdmGenerated`, and advances `Idle→Finalizing`. *(EMR/FHIR integration in F9/F10 remains 📋 Phases 6–8.)*
> - **G1–G6 → ✅ built:** section-persistence events (G1), one-button MDM (G2), medications + diagnoses + free-text data-reviewed (G3, scoped per Q4), disposition capture (G4), standalone `mdm_standalone` bin (G5), and unstubbed `lab`/`note`/`export` (G6).
> - **G7 (transcript compaction) and G8 (structured exam templates) remain ❌/📋 deferred** to later phases, as planned.
>
> The per-item Status cells below reflect the *original* analysis (2026-06-08); treat this banner as the authoritative current state. Evidence line numbers below predate the layered restructure (`src/models/…`, `src/cli.rs:NNN`) — see `CLAUDE.md` "Key Files" for current paths (`src/domain/`, `src/application/`, `src/interface/`).

### Sources read
- **Feedback:** `doctor-feedback-responses.md` (Round 1, the two original questions + our answers) and Dr. Joey's **Round 2** feedback supplied in the task brief (HPI+MDM workflow, re-eval skepticism, "MDM from chart data" as the game-changer, deep EMR integration). *Round 2 is not yet captured in any repo file — see Action 0.*
- **Product framing:** `product-summary.md`
- **Roadmap / planning:** `roadmap-and-next-phase-040826.md`, `phase2.md`, `ARCHITECTURE.md`, `creative-improvements.md`, `approck-feasibility-analysis.md`, `infrastructure-cost-model.md`, `pricing-strategy.md`, `state_pattern.md`
- **Source verified (not trusted from CLAUDE.md):** `src/main.rs`, `src/cli.rs`, `src/shift_manager.rs`, `src/encounter_driver.rs`, `src/events.rs`, `src/state.rs`, `src/summarizer.rs`, `src/claude_summarizer.rs`, `src/export.rs`, `src/deidentify.rs`, `src/models/{note,result,disposition,encounter,mod}.rs`, `src/bin/spike_summarizer.rs`

### Status legend
- ✅ **Implemented** — working in the live (runnable) path, with file/function cited
- 🟡 **Partial** — building blocks exist but not wired end-to-end, or only partially
- ❌ **Not implemented** — confirmed absent in code (by search, not by CLAUDE.md absence)
- 📋 **Planned, not built** — exists in a roadmap doc, no code yet

---

## 1. The single most important finding (read this first)

**The AI generation layer is completely disconnected from the live event-sourced pipeline.** This is the crux of every Dr. Joey item, so it's stated once here and referenced below.

- `ShiftManager` (`src/shift_manager.rs:17-25`) and `EncounterDriver` (`src/encounter_driver.rs:16-22`) hold **no** `Summarizer`. `main.rs` never constructs one. The only place `summarize_encounter` / `generate_mdm` run is the throwaway `src/bin/spike_summarizer.rs`.
- `ShiftManager::trigger_mdm()` (`src/shift_manager.rs:125-133`) publishes **only** a `TriggerMDM` state-transition event. It does **not** call `generate_mdm`. The `mdm` CLI command (`src/cli.rs:228-232`) prints "MDM triggered." and nothing is generated. `phase2.md:836-838` confirms this was a *deliberate* scope cut ("MDM triggered. Note phase: Finalizing.").
- There is **no event** that folds generated text back onto the note. `NoteEvent` (`src/events.rs:11-54`) has no `EncounterSummarized` / `MdmGenerated` / `DispositionSet` variant. So even when `generate_mdm` runs in the spike, its output is set on a *local* `NoteSections` (`src/bin/spike_summarizer.rs:64-65`) and never persisted to `PatientNote.note_sections` (`src/models/note.rs:103`).

**Consequence:** In the running app today, a patient note accumulates transcripts and lab results, but its `note_sections.hpi/ros/exam/mdm` are **never populated**. Dr. Joey's headline ask — "click a button, get an MDM from the chart" — has its component parts written (the Claude prompts are good) but is **not deliverable** because the wiring, the persistence events, and most of the structured inputs are missing.

---

## 2. Feedback inventory + implementation status

### Round 1 — `doctor-feedback-responses.md`

| # | Feedback item | Source | Status | Evidence |
|---|---|---|---|---|
| **R1** | *"Would it listen to all interactions… or would I turn it on/off and select the patient… I get pulled from room to room in quick succession."* — the listening model | `doctor-feedback-responses.md:10`, answered `:15-30` | ✅ **Implemented** (backend) | Patient-anchored quick switch: `ShiftManager::set_active_patient` closes any open encounter then switches context (`src/shift_manager.rs:86-106`); `EncounterDriver::switch_patient` stop→start atomically (`src/encounter_driver.rs:95-102`); CLI `switch`/`start`/`stop` (`src/cli.rs:179-221`). Apple Watch one-tap UI is the unbuilt front-end (expected). |
| **R2** | *"Would the software integrate directly with the EMR? Epic has Abridge… doesn't work well in the ED because encounters are discontinuous."* | `doctor-feedback-responses.md:11`, answered `:34-52` | 🟡 **Partial** / 📋 planned | Differentiator (multi-encounter merge) is real: `summarize_encounter` merges into prior sections (`src/claude_summarizer.rs:113-161`). EMR integration is **not** built — clipboard export only (`src/export.rs`); FHIR is roadmap (Phases 6-8, `roadmap-and-next-phase-040826.md:197-224`). |

### Round 2 — task-brief feedback (the deeper, more recent pass)

| # | Feedback item (quoted/paraphrased) | Status | Evidence / where it lives |
|---|---|---|---|
| **F1** | Workflow uses a dot-phrase with **smart links pulling structured chart data** — *"past medical history, surgical history, medications… ED workup, test results, medications administered, disposition and other objective data."* HPI and MDM are the most time-consuming parts. | ❌/🟡 | Structured results exist for **labs/imaging/vitals only** (`src/models/result.rs:31-36`). **No** PMH, PSH, medications, orders, or disposition-as-input modeled. `Disposition` type exists (`src/models/disposition.rs`) but is never set. |
| **F2** | Current real workflow: **Doximity AI Scribe → HPI**, then **copy note into ChatGPT → MDM**, AI filling narrative around existing chart data (dx, meds administered, labs/imaging, consults, dispo, follow-up). | (context, not a feature) | Motivates F3/F8/F9. |
| **F3** | Ideal = **single workflow**: scribe listens → HPI; then **auto-ingest objective chart data → complete MDM** (workup, treatments, results, consultations, diagnosis, disposition). Eliminate copy-paste. | ✅ **Built (Phase 3)** | ~~🟡 Partial~~ Now wired end-to-end: `stop_encounter` summarizes HPI/Exam and `trigger_mdm` generates the MDM from objective data. The "single workflow" exists in the backend (see Phase 3 banner). Originally: both halves existed as isolated library calls (`summarize_encounter`, `generate_mdm`) but neither was wired. |
| **F4** | Abridge's **narrative is saved and discoverable** — a medico-legal concern (the "turkey sandwich" anecdote: not everything said belongs in a permanent record). | 🟡 **Partial** / 📋 | De-id before API calls exists (`src/deidentify.rs`), but raw `TranscriptReady` payloads are stored **indefinitely** in the events table — the discoverability problem, *worsened* by event-sourcing. Flagged + fix designed (`TranscriptCompacted` tombstone) in `creative-improvements.md:193-216`, **not built**. |
| **F5** | **Re-eval skepticism:** *"most people would have a hard time using it consistently… you're getting pulled in a million directions… tough to remember to record every interaction."* | ✅ feature exists; concern **open** | The re-eval/merge feature is implemented (R1 + `summarize_encounter` merge). The *concern* is adoption friction. **Non-negotiable: keep it.** Response = make the product valuable even when re-eval is used sparingly (prioritize F8). |
| **F6** | Most **re-evaluations don't add much new information**; sometimes useful for documentation, often not. | (prioritization signal) | Reinforces F5. Don't make headline value depend on disciplined re-eval capture. |
| **F7** | To capture **physical exam findings you must verbalize them — not natural.** | ❌ built / 📋 designed | No structured/normal-exam capture in code. Tap-to-expand "normal templates" for exam is designed in `ARCHITECTURE.md:765-769` but unbuilt. |
| **F8** | **The game-changer: AI generates the MDM from the note already in the system** + objective data, meds, results, disposition — *"the biggest documentation pain point, and I don't think many people are doing it well."* Make it seamless. | ✅ **Built (Phase 3)** | ~~🟡/❌ Primary gap~~ `generate_mdm` is now wired through `trigger_mdm`: assembles results + meds + diagnoses + data-reviewed + disposition, de-identifies, generates, and persists via `MdmGenerated` (then `TriggerMDM` → Finalizing). Billing-aware prompt unchanged (2021 E/M). Originally: existed but unwired, output not persisted, inputs incomplete. |
| **F9** | Reiterated ideal: scribe → HPI, EMR-integrated; doc does normal care (orders, results, diagnoses, disposition); then **click a button in the MDM section → AI generates MDM** from documentation/orders/results/diagnoses/disposition. | ✅ **Built (Phase 3)**; EMR 📋 | ~~❌ end-to-end~~ The `mdm` button now generates from documentation + results + diagnoses + disposition (diagnoses modeled in Phase 3 G3). EMR integration remains 📋 Phases 6–8. Orders/consults captured as free-text `data_reviewed` (full FHIR modeling deferred). |
| **F10** | Deep EMR integration is the **biggest challenge** (needs EHR-vendor buy-in). Open question: *"could it work as a standalone prototype… before full integration?"* Direction = fully integrated. | ✅ **Prototype built (Phase 3 G5)**; product decision 📋 | `mdm_standalone` bin (`src/bin/mdm_standalone.rs`) answers the "standalone prototype before integration?" question concretely: pipe `<note> ---DATA--- <data> ---DISPO--- <dispo>` on stdin → generated MDM, no FHIR. Whether to ship it as a *product* (Q5) is still an open product decision. |

---

## 3. Cross-reference against the roadmap (so you don't double-plan)

These Dr. Joey items are **already covered by existing planning docs** — treat them as "scheduled," not new:

| Feedback | Already planned in | Notes |
|---|---|---|
| EMR integration (R2, F9, F10) | `roadmap-and-next-phase-040826.md:197-224` — Phase 6 FHIR READ (`Patient`/`Encounter`/`Observation`/`DiagnosticReport`, publishes `ResultReceived`), Phase 7 FHIR WRITE (`DocumentReference` push), Phase 8 deep Epic/HL7 | `ResultType`/`ResultSource` already FHIR-shaped (`src/models/result.rs:10-50`). Don't re-spec FHIR; the gap is the *pre-FHIR* path (G5). |
| Standalone-before-integration sequencing (F10) | `product-summary.md:64-71` integration table; clipboard MVP is the deliberate first step (`roadmap-and-next-phase-040826.md:89`) | The *thesis* exists; the specific standalone-MDM product (G5) does not. |
| Disposition capture (F1, F8) | `ARCHITECTURE.md:771-783` "Disposition Workflow" (dispo quick-tag bottom sheet feeding the MDM builder) | Designed, **not built** — becomes Gap G4. |
| MDM-from-data concept (F3, F8, F9) | `ARCHITECTURE.md:605-618` "MDM Builder" (chief complaint + labs + exam + disposition → MDM); cost model assumes regeneration (`infrastructure-cost-model.md:33,127`) | The *idea* is planned; the *wiring + persistence events* are not (G1, G2). `phase2.md:836-838` explicitly deferred real generation. |
| Exam normal-templates (F7) | `ARCHITECTURE.md:765-769` | Designed, not built (G8). |
| Transcript discoverability/PHI minimization (F4) | `creative-improvements.md:193-216` (`TranscriptCompacted` compaction) | Designed, not built (G7). |
| Lab / note / export CLI commands (prereqs for F8) | `phase2.md:826` (lab assembly), `:852` (note prints sections), `:862` (export) | Planned; currently stubbed in `src/cli.rs` (G6 — quick wins). |

---

## 4. Gap list (genuinely missing — neither implemented nor adequately planned)

Ordered by leverage on Dr. Joey's stated priorities.

- ✅ **G1 — Section-persistence events.** *(Built, Phase 3 Step 1.)* `EncounterSummarized` / `MdmGenerated` / `DispositionSet` (+ `MedicationRecorded` / `DiagnosisAssigned` / `DataReviewed`) added to `NoteEvent`, folded in `apply_event` (`src/domain/replay.rs`). Generated text now reaches the note.
- ✅ **G2 — One-button MDM, end-to-end.** *(Built, Phase 3 Step 6.)* `ShiftManager::trigger_mdm` assembles inputs, de-identifies, calls `generate_mdm`, persists `MdmGenerated`, then `TriggerMDM` (→ `Finalizing`). Guards on empty input + phase.
- ✅ **G3 — Objective-data inputs** *(Built, Phase 3 Step 4; scoped to meds + diagnoses per Q4.)* `Medication` (home/administered) and `Diagnosis` modeled (`src/domain/`); orders/consults captured as free-text `data_reviewed`. Full FHIR `ServiceRequest`/`Encounter` modeling deferred to Phase 6.
- ✅ **G4 — Disposition capture wired** *(Built, Phase 3 Step 3.)* `DispositionSet` event + `dispo` CLI command + `apply_event` + MDM/export feed.
- ✅ **G5 — Standalone "MDM-from-pasted-chart-data" path** *(Built, Phase 3 Step 7.)* `src/bin/mdm_standalone.rs`. Shipping it as a product is still open (Q5).
- ✅ **G6 — Unstub `lab`, `note`, `export` CLI handlers** *(Built, Phase 3 Step 2.)*
- ❌ **G7 — Discoverability / transcript-compaction controls** (F4). **Deferred** — designed in `creative-improvements.md`, not built.
- ❌ **G8 — Structured/normal exam capture without verbalizing** (F7). **Deferred** — designed in `ARCHITECTURE.md`, lowest priority for a CLI backend.

**Async-bridge prerequisite (cross-cutting):** the `Summarizer` trait is `async` via RPITIT (`impl Future`, `src/summarizer.rs:9-27`), which is **not `dyn`-compatible**, and `main.rs` is synchronous. Wiring any Claude call into the CLI requires bridging sync↔async (see Q1/Q2).

---

## 5. Detailed implementation plan for the gaps

The app is a **developer CLI backend** (`src/main.rs:104-117` welcome banner) — the surface for new capabilities is new `Command` variants + `ShiftManager` methods + `NoteEvent` variants folded in `apply_event`. The plan stays inside the existing event-sourced architecture.

### Prerequisite P0 — Bridge the sync CLI to the async summarizer · **S–M**
- **Approach:** Store a `tokio::runtime::Runtime` (or `Handle`) and a summarizer on `ShiftManager`; `block_on` at the command boundary. Follow the existing generic pattern: add `S: Summarizer` to `ShiftManager<A, T>` → `ShiftManager<A, T, S>` (mirrors how `A`/`T` are already threaded), avoiding the non-`dyn` trait problem.
- **Files:** `src/shift_manager.rs` (struct + `new`), `src/main.rs` (construct `ClaudeSummarizer` + runtime, inject). `src/cli.rs` handlers call the new sync-wrapper methods.
- **Open question:** Q1, Q2.

### G1 — Section-persistence events · **M** · depends on: none (do first)
- **Approach:** Add to `NoteEvent` (`src/events.rs:11-54`):
  - `EncounterSummarized { patient_id, encounter_id, sections_delta: NoteSections, source_encounter_ids: Vec<Uuid>, clinical_occurred_at }`
  - `MdmGenerated { patient_id, mdm_text: String, clinical_occurred_at }`
  - (`DispositionSet` lands in G4.)
- Extend `apply_event` (`src/state.rs:247-307`): merge `sections_delta` into `note.note_sections`, push a `NoteSectionVersion` into `note.section_history` (the field already exists, `src/models/note.rs:104`), bump `version`/`updated_at`, append an `AuditEntry`. `MdmGenerated` sets `note_sections.mdm`.
- Add constructors + arms to `extract_patient_id` / `event_type` (`src/events.rs:56-79`).
- **Files:** `src/events.rs`, `src/state.rs`, tests in `src/state.rs` + `src/tests/persistence_integration.rs`.
- **Why first:** every other generation gap persists through these events; replay/snapshot correctness depends on them.

### G2 — One-button MDM end-to-end · **M** · depends on: P0, G1, G4, G6
- **Approach:** Rework `ShiftManager::trigger_mdm` (`src/shift_manager.rs:125-133`) to:
  1. Read the active `PatientNote` from the store.
  2. Assemble inputs: `results_summary` from `note.results` (format labs/imaging/vitals), `disposition` from `note.disposition` (G4), and — once G3 lands — meds/orders/diagnoses/consults.
  3. `block_on(summarizer.generate_mdm(&note.note_sections, &results_summary, &disposition))`.
  4. Publish `MdmGenerated` (persists MDM via G1) **then** the existing `TriggerMDM` (drives `Idle→Finalizing`, `src/state.rs:114-116`). Order matters: persist text before/with the transition that snapshots `Finalizing` (`src/events.rs:164-188`).
- Mirror the same wiring for HPI/Exam: after `stop_encounter` produces `TranscriptReady` (`src/encounter_driver.rs:67-86`), call `summarize_encounter` and publish `EncounterSummarized`. *This is what finally makes notes populate in the live app.*
- **Files:** `src/shift_manager.rs`, `src/encounter_driver.rs` (or a new summarization step the manager orchestrates), `src/cli.rs:228-232` (report resulting phase + that MDM was generated).
- **Open question:** Q3 (run summarization inline on stop, or as an explicit command?).

### G3 — Objective-data models + events (meds, orders, diagnoses, consults) · **L** (or **M** if scoped to meds + diagnoses first) · depends on: G1
- **Approach:** New models mirroring the FHIR-shaped `result.rs` style:
  - `src/models/medication.rs` — `Medication { name, dose, route, rxnorm: Option<String>, kind: Home|Administered, given_at }`
  - `src/models/diagnosis.rs` — `Diagnosis { text, icd10: Option<String>, is_primary }`
  - orders & consults: start as lightweight free-text "data reviewed" entries (or `ResultType` extensions) before full modeling.
- New events: `MedicationRecorded`, `DiagnosisAssigned`, (`OrderRecorded`, `ConsultRecorded` later); fold into `apply_event`; add `Vec<Medication>` / `Vec<Diagnosis>` to `PatientNote` (`src/models/note.rs:90-110`).
- Feed all of it into the G2 input assembly and update the `generate_mdm` prompt (`src/claude_summarizer.rs:169-196`) to cite meds/dx/consults explicitly.
- New CLI commands: `med`, `dx`, (`order`, `consult`).
- **Files:** `src/models/{medication,diagnosis}.rs` + `mod.rs`, `src/events.rs`, `src/state.rs`, `src/cli.rs`, `src/claude_summarizer.rs`.
- **Open question:** Q4 (which types first; recommend **meds + diagnoses** — highest MDM signal per Dr. Joey).

### G4 — Disposition capture wired · **S** · depends on: G1
- **Approach:** Add `DispositionSet { patient_id, disposition: Disposition, clinical_occurred_at }` to `NoteEvent`; `apply_event` sets `note.disposition` (type already exists, `src/models/disposition.rs`). Add a `dispo` CLI command (e.g. `dispo discharge "<instructions>" [follow-up]`, `dispo admit <service>`, …). Feeds G2's MDM input and the already-present `format_note` disposition arg (`src/export.rs:42-43`).
- **Files:** `src/events.rs`, `src/state.rs`, `src/cli.rs`, `src/shift_manager.rs`.

### G5 — Standalone "MDM-from-pasted-chart-data" command · **S–M** · depends on: P0
- **Approach:** A `mdm-from-text` command (or `src/bin/` tool) that takes a pasted existing note + objective-data blob and calls `generate_mdm` directly — **no FHIR, no event sourcing required**. This is the fastest validation of Dr. Joey's F8/F10 thesis and a credible demo for other docs. Optionally accept a file path for the pasted chart.
- **Files:** `src/cli.rs` (new command) or `src/bin/mdm_standalone.rs`; reuses `ClaudeSummarizer::generate_mdm` unchanged.
- **Why valuable:** directly answers F10 ("could it work as a standalone prototype before full integration?") with a yes you can put in front of a clinician this week.
- **Open question:** Q5 (productize standalone, or treat as internal validation only?).

### G6 — Unstub `lab`, `note`, `export` CLI handlers · **S** · depends on: none
- **Approach:** The blockers cited in the stubs ("needs an active patient id accessor", `src/cli.rs:223-226,246-254`) **no longer exist** — `ShiftManager::active_patient_id()` (`:170-172`), `add_lab()` (`:108-123`), and `get_note()` (`:153-155`) are all present.
  - `lab` → build `ClinicalResult` (per `phase2.md:826`) + `manager.add_lab(...)`.
  - `note` → `manager.get_note(active)` → print sections/results/phase (`phase2.md:852`).
  - `export` → `format_note(...)` (`src/export.rs:5`) and print.
- **Files:** `src/cli.rs` only.
- **Why early:** these are how you *feed* and *observe* G2 during testing; cheap and unblock verification.

### G7 — Discoverability / transcript compaction · **L (mostly design)** · depends on: G1
- **Approach:** Implement the designed `TranscriptCompacted` tombstone lifecycle (`creative-improvements.md:204-216`): after sign + retention window, replace `TranscriptReady` payloads with a tombstone; replay tolerates it. Optionally an "ephemeral transcript" mode that summarizes-then-drops raw narrative.
- **Files:** `src/events.rs`, `src/state.rs` (replay tolerance), `src/repository.rs`/`src/db.rs` (compaction job), retention policy.
- **Open question:** Q6 — this *tensions* with the "immutable, medico-legally defensible audit trail" selling point (`product-summary.md:76`). Needs your call before coding.

### G8 — Structured/normal exam capture · **M** · depends on: G1
- **Approach:** Per `ARCHITECTURE.md:765-769`, tap-to-expand normal-system templates so exam isn't fully verbalized; in CLI terms, an `exam normal <systems>` / `exam <system> "<finding>"` command that emits an `EncounterSummarized`-style exam delta. Largely a front-end concern; minimal backend until the iOS layer exists.
- **Files:** `src/cli.rs`, plus the G1 event path.
- **Priority:** lowest — least relevant to a headless backend and to Dr. Joey's #1 ask.

### Suggested build order (dependency-aware)
```
P0 (async bridge) ─┬─ G1 (events) ─┬─ G4 (dispo) ─┐
G6 (unstub CLI) ───┘               ├─ G3 (meds/dx)─┼─ G2 (one-button MDM end-to-end)  ← headline value
G5 (standalone MDM) ── (P0 only, ship in parallel for fast clinician validation)
G7 (compaction) ── after G1, gated on Q6
G8 (exam templates) ── after G1, lowest priority
```
**Fastest path to demonstrating Dr. Joey's game-changer:** **G5** (this week, no dependencies beyond P0) to validate the thesis, then **P0→G1→G6→G4→G2** for the integrated, persisted version, with **G3** deepening MDM quality.

---

## 6. Open design questions for you (decide before coding)

- **Q1 — Sync↔async bridge.** Store a `tokio::runtime::Runtime` on `ShiftManager` and `block_on` Claude calls at the command boundary? (Recommended — keeps the CLI loop simple.) Or convert the `main.rs` loop to async?
- **Q2 — Summarizer injection.** Add a generic `S: Summarizer` to `ShiftManager` (matches the existing `A`/`T` generics; avoids the non-`dyn` RPITIT trait), or refactor `Summarizer` to a `dyn`-compatible `BoxFuture` trait? (Recommend the generic.)
- **Q3 — When does summarization run?** Inline on `stop_encounter` (auto-populate HPI/Exam as you go), or only on an explicit command? Inline is closer to Dr. Joey's "seamless," but costs an API call per encounter (cf. `infrastructure-cost-model.md:127`).
- **Q4 — Which objective-data types first (G3)?** Recommend **medications + diagnoses** (Dr. Joey stresses "medications administered" and "diagnosis"); defer orders/consults to free-text "data reviewed" initially.
- **Q5 — Standalone vs. integrated (F10/G5).** Build the standalone paste-in MDM generator as a real near-term product/prototype, or only as internal validation pending FHIR (Phases 6-7)? This is the central product call Dr. Joey is asking you to make.
- **Q6 — Discoverability vs. audit trail (F4/G7).** Do we offer ephemeral/compacted transcripts to address the "saved and discoverable" concern — knowing it tensions with the immutable-audit-trail selling point in `product-summary.md:76`?
- **Q7 — Pre-FHIR chart context.** Where do PMH/PSH/home-meds come from before Phase 6 — manual entry, paste (G5), or skip until FHIR read?

## 7. Ambiguities in the feedback (listed, not guessed)
- **A1 — "the note that's already in the system."** Dr. Joey means the EMR chart (Epic), not our app's note. Confirms the standalone path (G5) must accept *externally authored* text as input, not assume our app produced the HPI. (Drives Q5/Q7.)
- **A2 — Re-eval "nice but hard to use" (F5/F6).** Not a request to remove it (and it's a non-negotiable). Read as a prioritization signal: make MDM-from-data the headline so value doesn't hinge on disciplined re-eval capture. If you read it differently, say so.
- **A3 — "click a button in the MDM section" (F9).** Assumed to be Epic's MDM field in the deep-integration end-state, approximated today by our `mdm` command. Whether the near-term button lives in *our* app or via a paste-back into Epic depends on Q5.

## 8. Action 0 — capture this feedback in the repo
Round 2 currently lives only in chat. Recommend appending it to `doctor-feedback-responses.md` (or a `doctor-feedback-responses-2.md`) so the inventory above has a durable, line-citable source. *(Not done here — this report does not modify other files per the task constraint.)*
