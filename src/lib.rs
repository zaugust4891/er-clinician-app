pub mod audio;
pub mod claude_summarizer;
pub mod db;
pub mod deidentify;
pub mod encounter_driver;
pub mod error;
pub mod events;
pub mod export;
pub mod models;
pub mod repository;
pub mod shift_manager;
pub mod state;
pub mod stt;
pub mod summarizer;

#[cfg(test)]
mod tests;
