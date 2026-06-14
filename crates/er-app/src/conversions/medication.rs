use er_domain::MedicationKind;
use crate::error::{AppError, AppResult};

/// Parse a `med <home|given> <name> [dose] [route]` command into the parts
/// `ShiftManager::record_medication` needs. Dose/route are single tokens
/// (e.g. `med given Aspirin 324mg PO`) — sufficient for a developer CLI.
///
/// Returns `(kind, name, dose, route)`.
pub fn parse_medication(
    args: &[String],
) -> AppResult<(MedicationKind, String, String, Option<String>)> {
    if args.len() < 3 {
        return Err(AppError::CliError(
            "Usage: med <home|given> <name> [dose] [route]".to_string(),
        ));
    }

    let kind = match args[1].to_lowercase().as_str() {
        "home" => MedicationKind::Home,
        "given" => MedicationKind::AdministeredInEd,
        other => {
            return Err(AppError::CliError(format!(
                "Unknown medication kind: '{other}'. Use 'home' or 'given'."
            )));
        }
    };

    let name = args[2].clone();
    let dose = args.get(3).cloned().unwrap_or_default();
    let route = args.get(4).cloned();

    Ok((kind, name, dose, route))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn parses_given_medication_with_dose_and_route() {
        let (kind, name, dose, route) =
            parse_medication(&args(&["med", "given", "Aspirin", "324mg", "PO"])).unwrap();

        assert_eq!(kind, MedicationKind::AdministeredInEd);
        assert_eq!(name, "Aspirin");
        assert_eq!(dose, "324mg");
        assert_eq!(route, Some("PO".to_string()));
    }

    #[test]
    fn parses_home_medication_without_optional_fields() {
        let (kind, name, dose, route) =
            parse_medication(&args(&["med", "home", "Lisinopril"])).unwrap();

        assert_eq!(kind, MedicationKind::Home);
        assert_eq!(name, "Lisinopril");
        assert_eq!(dose, "");
        assert_eq!(route, None);
    }

    #[test]
    fn rejects_unknown_kind() {
        let err = parse_medication(&args(&["med", "iv", "Aspirin"]));
        assert!(err.is_err());
    }

    #[test]
    fn rejects_missing_name() {
        let err = parse_medication(&args(&["med", "given"]));
        assert!(err.is_err());
    }
}
