use rusqlite::{params, Connection, OptionalExtension};
use crate::error::{AppError, Result};
use crate::models::*;  // all your domain types

pub fn save_shift(conn: &Connection, shift: &Shift) -> Result<()> {
    let data = serde_json::to_string(shift)?;
    let id = shift.id.to_string();
    let started_at = shift.started_at.to_rfc3339();
    let ended_at = shift.ended_at.map(|t| t.to_rfc3339());
    conn.execute(
        "INSERT OR REPLACE INTO shifts (id, started_at, ended_at, data) VALUES (?, ?, ?, ?)",
        params![id, started_at, ended_at, data],
    )?;
    Ok(())
}

pub fn get_shift(conn: &Connection, id: ShiftId) -> Result<Shift> {
    let id_str = id.to_string();
    let json: String = conn.query_row(
        "SELECT data FROM shifts WHERE id = ?1",
        params![id_str],
        |row| row.get(0),
    )?;
    let shift: Shift = serde_json::from_str(&json)?;
    Ok(shift)
}

pub fn get_active_shift(conn: &Connection) -> Result<Option<Shift>> {
    let json: Option<String> = conn.query_row(
        "SELECT data FROM shifts WHERE ended_at IS NULL
        ORDER BY started_at DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .optional()?;

    json.map(|j| serde_json::from_str::<Shift>(&j))
        .transpose()
        .map_err(AppError::Serialization)
}
