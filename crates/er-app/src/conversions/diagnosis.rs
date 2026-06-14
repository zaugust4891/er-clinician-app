use crate::error::{AppError, AppResult};

/// Parse a `dx <text> [--primary] [icd10]` command into the parts
/// `ShiftManager::assign_diagnosis` needs.
///
/// - `--primary` may appear anywhere and marks the diagnosis primary.
/// - A trailing token that looks like an ICD-10 code (e.g. `I26.99`) is
///   taken as the code; everything else is the free-text diagnosis.
/// - The diagnosis text may be wrapped in quotes (`dx "Acute PE"`).
///
/// Returns `(text, icd10_code, is_primary)`.
pub fn parse_diagnosis(args: &[String]) -> AppResult<(String, Option<String>, bool)> {
    if args.len() < 2 {
        return Err(AppError::CliError(
            "Usage: dx <text> [--primary] [icd10]".to_string(),
        ));
    }

    let mut tokens: Vec<String> = args[1..].to_vec();

    let is_primary = tokens.iter().any(|t| t == "--primary");
    tokens.retain(|t| t != "--primary");

    let icd10_code = tokens
        .last()
        .filter(|t| looks_like_icd10(t))
        .cloned()
        .inspect(|_| {
            tokens.pop();
        });

    let text = tokens.join(" ").trim_matches('"').to_string();
    if text.is_empty() {
        return Err(AppError::CliError(
            "Diagnosis text is required: dx <text> [--primary] [icd10]".to_string(),
        ));
    }

    Ok((text, icd10_code, is_primary))
}

/// Heuristic: an ICD-10 code starts with an uppercase letter followed by a
/// digit (e.g. `I26`, `J18.9`) and otherwise contains only alphanumerics/dots.
/// Good enough to distinguish an explicit trailing code from diagnosis words.
fn looks_like_icd10(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_uppercase()
        && bytes[1].is_ascii_digit()
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn parses_quoted_text_with_primary_flag() {
        let (text, icd10, is_primary) =
            parse_diagnosis(&args(&["dx", "\"Acute", "PE\"", "--primary"])).unwrap();

        assert_eq!(text, "Acute PE");
        assert_eq!(icd10, None);
        assert!(is_primary);
    }

    #[test]
    fn parses_trailing_icd10_code() {
        let (text, icd10, is_primary) =
            parse_diagnosis(&args(&["dx", "Acute", "PE", "--primary", "I26.99"])).unwrap();

        assert_eq!(text, "Acute PE");
        assert_eq!(icd10, Some("I26.99".to_string()));
        assert!(is_primary);
    }

    #[test]
    fn plain_text_without_flags_is_not_primary() {
        let (text, icd10, is_primary) =
            parse_diagnosis(&args(&["dx", "non-cardiac", "chest", "pain"])).unwrap();

        assert_eq!(text, "non-cardiac chest pain");
        assert_eq!(icd10, None);
        assert!(!is_primary);
    }

    #[test]
    fn rejects_empty_text() {
        let err = parse_diagnosis(&args(&["dx", "--primary"]));
        assert!(err.is_err());
    }
}
