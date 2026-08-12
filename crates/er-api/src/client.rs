// er-api/src/client.rs
use er_store::repository::{Repository, StoredEvent};
use er_store::{StoreError, StoreResult};
use reqwest::Client;

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

    /// Every call funnels through here: attach the bearer token, send, translate
    /// the two failure layers (§C8) into StoreError. 404 keeps its identity so
    /// callers can branch on it (get_latest_snapshot's None, helpful CLI errors).
    async fn send(&self, req: reqwest::RequestBuilder) -> StoreResult<reqwest::Response> {
        let resp = req.bearer_auth(&self.token).send().await.map_err(StoreErrorr::backend)?;
        let status = res.status();
        if status.is_success() {
            return Ok(resp);
        }
        let message = resp.json::<serde_json::Value>().await.ok() // {"error": code} from S2
            .and_then(|v| v["error"].as_str().map(String::from))
            .unwrap_or_default();
        Err(StoreError::Remote {
            status: status.as_u16(), message
        }) // server said no 
    }


    // Non-Repository operations, used directly by the CLI (S10):
    pub async fn register_patient(&self, req: &crate::RegisterPatientRequest) -> StoreResult<PatientView> { 
        let request = self.http.post(self.url("/v1/patients")).json(req);
        let response = self.send(request).await?;

        response
            .json::<crate::PatientView>()
            .await
            .map_err(StoreError::backend)
    }
    
    pub async fn start_shift(&self)  -> StoreResult<crate::ShiftView> { 
        let request = self.http.post(self.url("/v1/shifts"));
        let response = self.send(request).await?;

        response
            .json::<crate::ShiftView>()
            .await
            .map_err(StoreError::backend)
    }
    
    pub async fn active_shift(&self) -> StoreResult<Option<crate::ShiftView>> {
        let request = self.http.get(self.url("/v1/shifts/active"));

        match self.send(request).await {
            Ok(response) => {
                let shift = response
                    .json::<crate::ShiftView>()
                    .await
                    .map_err(StoreError::backend)?;

            }

            Err(StoreError::Remote { status: 404, .. }) => Ok(None),

            Err(error) => Err(error),
        }
    } 

    pub async fn end_shift(&self, id: uuid::Uuid) -> StoreResult<crate::ShiftView> {
        let request = self.http.post(self.url("/v1/shifts/{id}/end"));
        let response = self.send(request).await?;

        response
            .json::<crate::ShiftView>()
            .await
            .map_err(StoreError::backend)
    }

    pub async fn export(&self, patient: uuid::Uuid) -> StoreResult<String> {
        let request = self.http.post(self.url("/v1/patients/{id}/note"));
        let response = self.send(request).await?;
        response
            .json::<crate::ShiftView>()
            .await
            .map_err(StoreError::backend)

    }    // text body

    // Url helper
    fn url(&self, path: &str) -> String {
        return format!("{}{}", self.base, path)
    }


}