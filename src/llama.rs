use anyhow::{Result, Context};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlamaError {
    #[error("Ollama service is not available: {0}")]
    ServiceUnavailable(String),
    
    #[error("Model '{0}' is not available: {1}")]
    ModelNotAvailable(String, String),
    
    #[error("Request failed: {0}")]
    RequestFailed(String),
    
    #[error("Failed to parse response: {0}")]
    ResponseParseError(String),
}

#[derive(Debug, Clone)]
pub struct LlamaClient {
    client: Client,
    base_url: String,
    model: String,
}

#[derive(Debug, Serialize)]
struct CompletionRequest {
    model: String,
    prompt: String,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    response: String,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
}

impl LlamaClient {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: "http://localhost:11434/api".to_string(),
            model: model.into(),
        }
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    pub async fn generate(&self, prompt: &str) -> Result<String> {
        let request = CompletionRequest {
            model: self.model.clone(),
            prompt: prompt.to_string(),
            temperature: 0.7,
        };

        let response = self.client
            .post(format!("{}/generate", self.base_url))
            .json(&request)
            .send()
            .await
            .context("Failed to connect to Ollama service")
            .map_err(|e| LlamaError::ServiceUnavailable(e.to_string()))?;

        match response.status() {
            StatusCode::OK => {
                let completion: CompletionResponse = response
                    .json()
                    .await
                    .context("Failed to parse response")
                    .map_err(|e| LlamaError::ResponseParseError(e.to_string()))?;
                Ok(completion.response)
            }
            StatusCode::NOT_FOUND => {
                let error: ErrorResponse = response
                    .json()
                    .await
                    .context("Failed to parse error response")
                    .map_err(|e| LlamaError::ResponseParseError(e.to_string()))?;
                Err(LlamaError::ModelNotAvailable(
                    self.model.clone(),
                    error.error,
                ).into())
            }
            status => {
                let error = match response.text().await {
                    Ok(text) => text,
                    Err(_) => format!("Request failed with status: {}", status),
                };
                Err(LlamaError::RequestFailed(error).into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_basic_completion() -> Result<()> {
        let client = LlamaClient::new("llama2");
        let response = client.generate("What is Rust?").await?;
        assert!(!response.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_model() {
        let client = LlamaClient::new("non_existent_model");
        let result = client.generate("Test prompt").await;
        assert!(matches!(result.unwrap_err().downcast_ref(),
            Some(LlamaError::ModelNotAvailable(_, _))));
    }

    #[tokio::test]
    async fn test_service_unavailable() {
        let mut client = LlamaClient::new("llama2");
        client.base_url = "http://localhost:11111/api".to_string(); // Wrong port
        let result = client.generate("Test prompt").await;
        assert!(matches!(result.unwrap_err().downcast_ref(),
            Some(LlamaError::ServiceUnavailable(_))));
    }
} 