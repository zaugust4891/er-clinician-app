//! Claude Messages API wire types and transport selection.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{AiError, AiResult};

const DIRECT_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
#[cfg(feature = "bedrock")]
const BEDROCK_VERSION: &str = "bedrock-2023-05-31";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_version: Option<&'static str>,
    pub max_tokens: u32,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct MessagesResponse {
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Deserialize)]
pub struct ContentBlock {
    pub text: Option<String>,
}

impl MessagesResponse {
    pub fn first_text(&self) -> Option<&str> {
        self.content.iter().find_map(|block| block.text.as_deref())
    }
}

/// Direct Anthropic API for local synthetic development, or Bedrock with IAM
/// authentication for staging and production.
pub enum ClaudeTransport {
    DirectApi {
        client: reqwest::Client,
        api_key: String,
        endpoint: String,
    },
    #[cfg(feature = "bedrock")]
    Bedrock {
        client: aws_sdk_bedrockruntime::Client,
    },
}

impl std::fmt::Debug for ClaudeTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DirectApi { endpoint, .. } => f
                .debug_struct("DirectApi")
                .field("endpoint", endpoint)
                .field("api_key", &"[REDACTED]")
                .finish(),
            #[cfg(feature = "bedrock")]
            Self::Bedrock { .. } => f.debug_struct("Bedrock").finish(),
        }
    }
}

impl ClaudeTransport {
    pub fn direct(api_key: impl Into<String>) -> AiResult<Self> {
        Self::direct_at(api_key, DIRECT_MESSAGES_URL)
    }

    fn direct_at(api_key: impl Into<String>, endpoint: impl Into<String>) -> AiResult<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(AiError::Refused {
                status: 0,
                message: "Anthropic API key must not be blank".to_string(),
            });
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|_| AiError::Transport("could not build the AI HTTP client".to_string()))?;
        Ok(Self::DirectApi {
            client,
            api_key,
            endpoint: endpoint.into(),
        })
    }

    #[cfg(feature = "bedrock")]
    pub fn bedrock(client: aws_sdk_bedrockruntime::Client) -> Self {
        Self::Bedrock { client }
    }

    /// Send one Messages API request and retain the provider's usage counters.
    pub async fn messages(
        &self,
        model: &str,
        mut request: MessagesRequest,
    ) -> AiResult<MessagesResponse> {
        match self {
            Self::DirectApi {
                client,
                api_key,
                endpoint,
            } => {
                request.model = Some(model.to_string());
                request.anthropic_version = None;
                let response = client
                    .post(endpoint)
                    .header("x-api-key", api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .json(&request)
                    .send()
                    .await
                    .map_err(|_| AiError::Transport("Anthropic request failed".to_string()))?;

                let status = response.status().as_u16();
                if !response.status().is_success() {
                    // Do not read or forward the provider body: it can echo prompt
                    // material and has no value in a clinician-facing error.
                    return Err(classify_status(status));
                }

                response.json().await.map_err(|_| {
                    AiError::Transport("Anthropic returned an invalid response".to_string())
                })
            }
            #[cfg(feature = "bedrock")]
            Self::Bedrock { client } => {
                request.model = None;
                request.anthropic_version = Some(BEDROCK_VERSION);
                let body = serde_json::to_vec(&request).map_err(|_| {
                    AiError::Summarization("could not encode the model request".to_string())
                })?;
                let response = client
                    .invoke_model()
                    .model_id(model)
                    .content_type("application/json")
                    .accept("application/json")
                    .body(aws_sdk_bedrockruntime::primitives::Blob::new(body))
                    .send()
                    .await
                    .map_err(|error| {
                        error
                            .as_service_error()
                            .map(classify_bedrock_error)
                            .unwrap_or_else(|| {
                                AiError::Transport("Bedrock invocation failed".to_string())
                            })
                    })?;

                serde_json::from_slice(response.body.as_ref()).map_err(|_| {
                    AiError::Transport("Bedrock returned an invalid response".to_string())
                })
            }
        }
    }
}

fn classify_status(status: u16) -> AiError {
    match status {
        429 => AiError::RateLimited("AI provider rate limit".to_string()),
        500..=599 => AiError::Transport(format!("AI provider unavailable (HTTP {status})")),
        _ => AiError::Refused {
            status,
            message: "AI provider refused the request".to_string(),
        },
    }
}

#[cfg(feature = "bedrock")]
fn classify_bedrock_error(
    error: &aws_sdk_bedrockruntime::operation::invoke_model::InvokeModelError,
) -> AiError {
    if error.is_throttling_exception() || error.is_service_quota_exceeded_exception() {
        return AiError::RateLimited("Bedrock rate limit or quota".to_string());
    }
    let status = if error.is_access_denied_exception() {
        Some(403)
    } else if error.is_resource_not_found_exception() {
        Some(404)
    } else if error.is_validation_exception() {
        Some(400)
    } else {
        None
    };
    match status {
        Some(status) => AiError::Refused {
            status,
            message: "Bedrock refused the request".to_string(),
        },
        None => AiError::Transport("Bedrock invocation failed".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_parsed_not_discarded() {
        let response: MessagesResponse = serde_json::from_value(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "usage": {
                "input_tokens": 41,
                "output_tokens": 7,
                "cache_read_input_tokens": 2_317,
                "cache_creation_input_tokens": 0
            }
        }))
        .unwrap();
        assert_eq!(response.usage.input_tokens, 41);
        assert_eq!(response.usage.output_tokens, 7);
        assert_eq!(response.usage.cache_read_input_tokens, 2_317);
        assert_eq!(response.usage.cache_creation_input_tokens, 0);
    }

    #[test]
    fn missing_optional_usage_counters_default_to_zero() {
        let response: MessagesResponse = serde_json::from_value(serde_json::json!({
            "content": [{"text": "ok"}],
            "usage": {"input_tokens": 4, "output_tokens": 2}
        }))
        .unwrap();
        assert_eq!(response.usage.cache_read_input_tokens, 0);
        assert_eq!(response.usage.cache_creation_input_tokens, 0);
    }

    #[test]
    fn a_400_is_refused_and_a_429_is_retryable() {
        assert!(matches!(
            classify_status(400),
            AiError::Refused { status: 400, .. }
        ));
        assert!(matches!(classify_status(429), AiError::RateLimited(_)));
        assert!(matches!(classify_status(503), AiError::Transport(_)));
    }

    #[test]
    fn direct_transport_debug_redacts_the_key() {
        let transport = ClaudeTransport::direct("secret-key").unwrap();
        let rendered = format!("{transport:?}");
        assert!(!rendered.contains("secret-key"));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[cfg(feature = "bedrock")]
    #[test]
    fn bedrock_validation_is_permanent_and_throttling_is_retryable() {
        use aws_sdk_bedrockruntime::operation::invoke_model::InvokeModelError;
        use aws_sdk_bedrockruntime::types::error::{ThrottlingException, ValidationException};

        let validation = InvokeModelError::ValidationException(
            ValidationException::builder().message("invalid").build(),
        );
        assert!(matches!(
            classify_bedrock_error(&validation),
            AiError::Refused { status: 400, .. }
        ));

        let throttled = InvokeModelError::ThrottlingException(
            ThrottlingException::builder().message("slow down").build(),
        );
        assert!(matches!(
            classify_bedrock_error(&throttled),
            AiError::RateLimited(_)
        ));
    }
}
