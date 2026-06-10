use crate::error::{AppError, AppResult};
use crate::domain::Disposition;

pub fn parse_disposition(args: &[String]) -> AppResult<Disposition> {
    if args.len() < 2 {
        return Err(AppError::CliError(
            "Usage: dispo <discharge|admit|transfer|ama|lwbs> ...".to_string(),
        ));
    }

    match args[1].to_lowercase().as_str() {
        "discharge" => {
            if args.len() < 3 {
                return Err(AppError::CliError(
                    "Usage: dispo discharge <instructions> [<follow-up>]".to_string(),
                ));
            }
            let fields = parse_free_text_fields(&args[2..]);
            Ok(Disposition::Discharge {
                instructions: fields[0].clone(),
                follow_up: fields.get(1).cloned(),
            })
        }
        "admit" => {
            if args.len() < 3 {
                return Err(AppError::CliError(
                    "Usage: dispo admit <service> [<bed>]".to_string(),
                ));
            }
            Ok(Disposition::Admit {
                service: args[2].clone(),
                bed: args.get(3).cloned(),
            })
        }
        "transfer" => {
            if args.len() < 3 {
                return Err(AppError::CliError(
                    "Usage: dispo transfer <destination> [<reason>]".to_string(),
                ));
            }
            Ok(Disposition::Transfer {
                destination: strip_wrapping_quotes(&args[2]),
                reason: if args.len() > 3 {
                    Some(parse_free_text_fields(&args[3..]).join(" "))
                } else {
                    None
                },
            })
        }
        "ama" => Ok(Disposition::AMA),
        "lwbs" => Ok(Disposition::LWBS),
        other => Err(AppError::CliError(format!("Unknown disposition: {other}"))),
    }
}

pub fn format_disposition_for_cli(disposition: &Disposition) -> String {
    match disposition {
        Disposition::Discharge {
            instructions,
            follow_up,
        } => match follow_up {
            Some(follow_up) => {
                format!("Discharge (instructions: {instructions}; follow-up: {follow_up})")
            }
            None => format!("Discharge (instructions: {instructions})"),
        },
        Disposition::Admit { service, bed } => match bed {
            Some(bed) => format!("Admit to {service} (bed: {bed})"),
            None => format!("Admit to {service}"),
        },
        Disposition::Transfer {
            destination,
            reason,
        } => match reason {
            Some(reason) => format!("Transfer to {destination} (reason: {reason})"),
            None => format!("Transfer to {destination}"),
        },
        Disposition::AMA => "AMA".to_string(),
        Disposition::LWBS => "LWBS".to_string(),
    }
}

fn parse_free_text_fields(tokens: &[String]) -> Vec<String> {
    let has_quoted_span = tokens.iter().any(|token| token.starts_with('"'));
    if !has_quoted_span {
        return vec![tokens.join(" ")];
    }

    let mut fields = Vec::new();
    let mut current = Vec::new();
    let mut in_quotes = false;

    for token in tokens {
        let starts_quote = token.starts_with('"');
        let ends_quote = token.ends_with('"') && token.len() > usize::from(starts_quote);

        if starts_quote && !in_quotes {
            in_quotes = true;
            current.clear();
        }

        let cleaned = token.trim_matches('"');
        if in_quotes {
            current.push(cleaned.to_string());
            if ends_quote {
                fields.push(current.join(" "));
                current.clear();
                in_quotes = false;
            }
        } else {
            fields.push(cleaned.to_string());
        }
    }

    if !current.is_empty() {
        fields.push(current.join(" "));
    }

    fields
}

fn strip_wrapping_quotes(value: &str) -> String {
    value.trim_matches('"').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn parses_discharge_with_quoted_instruction_and_follow_up() {
        let disposition = parse_disposition(&args(&[
            "dispo",
            "discharge",
            "\"ibuprofen",
            "as",
            "needed\"",
            "\"Cardiology",
            "1wk\"",
        ]))
        .unwrap();

        assert_eq!(
            disposition,
            Disposition::Discharge {
                instructions: "ibuprofen as needed".to_string(),
                follow_up: Some("Cardiology 1wk".to_string()),
            }
        );
    }

    #[test]
    fn parses_transfer_reason_tail() {
        let disposition = parse_disposition(&args(&[
            "dispo",
            "transfer",
            "MGH",
            "higher",
            "level",
            "of",
            "care",
        ]))
        .unwrap();

        assert_eq!(
            disposition,
            Disposition::Transfer {
                destination: "MGH".to_string(),
                reason: Some("higher level of care".to_string()),
            }
        );
    }
}
