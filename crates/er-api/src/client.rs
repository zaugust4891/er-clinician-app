// er-api/src/client.rs
use er_store::repository::{Repository, StoredEvent};
use er_store::{StoreError, StoreResult};

pub struct ServerApi {
    base: String,
    token: String,
    http: reqwest::Client,
}


impl ServerApi {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_owned();
        Self {
            base: base,
            token: token.into(),
            http: reqwest::Client::new()
        }
    }

    



}