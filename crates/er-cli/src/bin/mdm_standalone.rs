//! Phase 3 Step 7 — standalone `mdm-from-text` (G5 / Dr. Joey F10).
//!
//! The cheapest possible demo of the thesis: paste an existing note + chart
//! data on stdin, get a generated MDM. No audio, no event sourcing, no FHIR,
//! no shift state — just the Claude MDM prompt.
//!
//! Input format (all sections optional, order fixed):
//!
//! ```text
//! <existing HPI / exam prose>
//! ---DATA---
//! <objective data: results, meds, dx, orders/consults>
//! ---DISPO---
//! <disposition, e.g. "discharge home">
//! ```
//!
//! - Text before `---DATA---` becomes the HPI.
//! - Text between `---DATA---` and `---DISPO---` is the objective-data blob.
//! - Text after `---DISPO---` is the disposition; if omitted, "Not specified".
//! - If `---DATA---` is absent, the entire input is treated as the note.
//!
//! Usage:
//! ```bash
//! cargo run --bin mdm_standalone < chart_excerpt.txt
//! # or
//! printf 'HPI...\n---DATA---\nTroponin negative\n---DISPO---\ndischarge\n' \
//!     | cargo run --bin mdm_standalone
//! ```

use std::io::Read;

use er_domain::NoteSections;
use er_app::error::AppResult;
use er_ai::claude::ClaudeSummarizer;
use er_ai::summarizer::Summarizer;

const DATA_SEP: &str = "---DATA---";
const DISPO_SEP: &str = "---DISPO---";

#[tokio::main]
async fn main() -> AppResult<()> {
    dotenvy::dotenv().ok(); // load ANTHROPIC_KEY before constructing the client

    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;

    let (note_text, data_text, dispo) = split_input(&raw);

    if note_text.is_empty() && data_text.is_empty() {
        eprintln!(
            "No input. Pipe a note in, optionally with `{DATA_SEP}` and `{DISPO_SEP}` sections:\n\
             \n  printf 'HPI...\\n{DATA_SEP}\\nTroponin negative\\n{DISPO_SEP}\\ndischarge\\n' \
             | cargo run --bin mdm_standalone"
        );
        return Ok(());
    }

    let summarizer = ClaudeSummarizer::new()?;
    let sections = NoteSections {
        hpi: (!note_text.is_empty()).then_some(note_text),
        ros: None,
        exam: None,
        mdm: None,
        disposition_text: None,
    };

    let mdm = summarizer
        .generate_mdm(&sections, &data_text, &dispo)
        .await?;

    println!("\n===== GENERATED MDM =====\n{mdm}");
    Ok(())
}

/// Split raw stdin into `(note_text, data_text, disposition)`.
///
/// Pure and side-effect-free so the parsing contract can be unit-tested
/// without stdin or a network call (the Claude call itself is manual-only).
/// All three parts are trimmed; disposition falls back to "Not specified".
fn split_input(raw: &str) -> (String, String, String) {
    let (note_part, rest) = match raw.split_once(DATA_SEP) {
        Some((note, rest)) => (note, rest),
        None => (raw, ""),
    };

    let (data_part, dispo_part) = match rest.split_once(DISPO_SEP) {
        Some((data, dispo)) => (data, dispo),
        None => (rest, ""),
    };

    let note_text = note_part.trim().to_string();
    let data_text = data_part.trim().to_string();
    let dispo = {
        let trimmed = dispo_part.trim();
        if trimmed.is_empty() {
            "Not specified".to_string()
        } else {
            trimmed.to_string()
        }
    };

    (note_text, data_text, dispo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_all_three_sections() {
        let raw = "44yo M with chest pain.\n\
                   ---DATA---\n\
                   Troponin negative; CTA negative for PE; ASA 324 given\n\
                   ---DISPO---\n\
                   discharge home";

        let (note, data, dispo) = split_input(raw);

        assert_eq!(note, "44yo M with chest pain.");
        assert_eq!(
            data,
            "Troponin negative; CTA negative for PE; ASA 324 given"
        );
        assert_eq!(dispo, "discharge home");
    }

    #[test]
    fn data_present_but_no_dispo_defaults() {
        let raw = "HPI text\n---DATA---\nTroponin negative";

        let (note, data, dispo) = split_input(raw);

        assert_eq!(note, "HPI text");
        assert_eq!(data, "Troponin negative");
        assert_eq!(dispo, "Not specified");
    }

    #[test]
    fn no_separators_treats_everything_as_note() {
        let raw = "Just the HPI, nothing else.";

        let (note, data, dispo) = split_input(raw);

        assert_eq!(note, "Just the HPI, nothing else.");
        assert_eq!(data, "");
        assert_eq!(dispo, "Not specified");
    }

    #[test]
    fn trims_whitespace_around_separators() {
        let raw = "\n  HPI  \n\n---DATA---\n\n  data line  \n\n---DISPO---\n\n  admit  \n";

        let (note, data, dispo) = split_input(raw);

        assert_eq!(note, "HPI");
        assert_eq!(data, "data line");
        assert_eq!(dispo, "admit");
    }

    #[test]
    fn empty_input_yields_empty_note_and_data() {
        let (note, data, dispo) = split_input("   \n  ");

        assert_eq!(note, "");
        assert_eq!(data, "");
        assert_eq!(dispo, "Not specified");
    }
}
