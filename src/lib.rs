pub mod application;
pub mod audio;
pub mod claude_summarizer;
pub mod cli;
pub mod conversions;
pub mod db;
pub mod deidentify;
pub mod domain;
pub mod encounter_driver;
pub mod error;
pub mod export;
pub mod persistence;
pub mod repository;
pub mod shift_manager;
pub mod stt;
pub mod summarizer;

#[cfg(test)]
mod tests;
