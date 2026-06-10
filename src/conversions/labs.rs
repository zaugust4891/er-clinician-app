use crate::error::{AppResult, AppError};



pub fn parse_string_to_f64(input: String) -> AppResult<f64> {
    match input.parse::<f64>() {
        Ok(num) => Ok(num),
        Err(e) => Err(AppError::parse_error(
            format!("failed to parse input String into f64 '{}'", input),
            e,
        )),
    }
}