use anyhow::{Result, Context};
use reqwest::{
    Client, 
    Response,
    header::{HeaderMap, HeaderValue, CONTENT_TYPE, AUTHORIZATION}
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use crate::config::{Config, ModelConfig};
use log::{debug, warn, error, info};

#[derive(Debug, Error)]
pub enum LlamaError {
    #[error("Service is not available: {0}")]
    ServiceUnavailable(String),
    
    #[error("Model '{0}' is not available: {1}")]
    ModelNotAvailable(String, String),
    
    #[error("Request failed: {0}")]
    RequestFailed(String),
    
    #[error("Failed to parse response: {0}")]
    ResponseParseError(String),
    
    #[error("Authentication failed: {0}")]
    AuthenticationError(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    temperature: f32,
    max_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    #[serde(default)]
    response: String,          // For Ollama
    #[serde(default)]
    choices: Vec<Choice>,      // For OpenAI/Deepseek
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    delta: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Clone)]
pub struct LlamaClient {
    client: Client,
    pub config: ModelConfig,
}

unsafe impl Send for LlamaClient {}
unsafe impl Sync for LlamaClient {}

impl LlamaClient {
    pub fn new(config: ModelConfig) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            config,
        })
    }

    pub async fn generate(&self, messages: &[Message]) -> Result<Response> {
        info!("Generating response using model: {}", self.config.model);
        debug!("API URL: {}", self.config.api_url);
        
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: messages.to_vec(),
            stream: self.config.stream.unwrap_or_else(|| true),
            temperature: self.config.temperature.unwrap_or(0.7),
            max_tokens: self.config.max_tokens,
        };
        
        debug!("Request payload: {:?}", request);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        
        if let Some(api_key) = &self.config.api_key {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", api_key))
                    .map_err(|e| LlamaError::AuthenticationError(e.to_string()))?
            );
        }

        info!("Sending request to LLM API...");
        let response = self.client
            .post(&self.config.api_url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .context("Failed to connect to service")
            .map_err(|e| {
                error!("API connection failed: {}", e);
                LlamaError::ServiceUnavailable(e.to_string())
            })?;

        debug!("Response status: {}", response.status());
        debug!("Response headers: {:#?}", response.headers());
        
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!("API request failed with status {}: {}", status, body);
            return Err(LlamaError::RequestFailed(format!("Status: {}, Body: {}", status, body)).into());
        }
        
        info!("Received successful response from LLM API");
        
        Ok(response)
    }

    // Helper method to extract text from a response
    pub async fn get_response_text(response: Response) -> Result<String> {
        debug!("Parsing LLM response...");
        let completion: CompletionResponse = response
            .json()
            .await
            .context("Failed to parse response")
            .map_err(|e| {
                error!("Failed to parse LLM response: {}", e);
                LlamaError::ResponseParseError(e.to_string())
            })?;

        // Handle different response formats
        if !completion.response.is_empty() {
            // Ollama format
            Ok(completion.response)
        } else if let Some(choice) = completion.choices.first() {
            // OpenAI/Deepseek format
            if let Some(message) = &choice.message {
                Ok(message.content.clone())
            } else if let Some(delta) = &choice.delta {
                Ok(delta.content.clone())
            } else {
                Err(LlamaError::ResponseParseError("No content in response".to_string()).into())
            }
        } else {
            Err(LlamaError::ResponseParseError("Empty response".to_string()).into())
        }
    }

    pub fn set_provider(config: &Config, provider: &str) -> Result<Self> {
        info!("Switching to provider: {}", provider);
        
        // Get the model config for the provider
        let model_config = match provider {
            "deepseek" => config.deepseek.clone(),
            "openai" => config.openai.clone(),
            "llamacpp" => config.llamacpp.clone(),
            "ollama" => config.ollama.clone(),
            _ => {
                error!("Unsupported provider: {}", provider);
                return Err(anyhow::anyhow!("Unsupported provider: {}", provider))
            }
        };

        // Check for API key if needed
        if let Some(api_key) = &model_config.api_key {
            if api_key.contains("your-") {
                return Err(anyhow::anyhow!("Please set your API key for {} in the config file", provider));
            }
        }

        println!("Switched to {} provider", provider);
        println!("Using model: {}", model_config.model);
        println!("API URL: {}", model_config.api_url);
        
        // Print any custom settings that override defaults
        let defaults = &config.default;
        if let Some(temp) = model_config.temperature {
            if temp != defaults.temperature {
                println!("Temperature: {} (custom)", temp);
            }
        }
        if let Some(tokens) = model_config.max_tokens {
            if tokens != defaults.max_tokens {
                println!("Max tokens: {} (custom)", tokens);
            }
        }
        if let Some(stream) = model_config.stream {
            if stream != defaults.stream {
                println!("Stream: {} (custom)", stream);
            }
        }

        Self::new(model_config)
    }

    pub async fn test_availability(&self) -> Result<bool> {
        let test_message = vec![Message {
            role: "user".to_string(),
            content: "test".to_string(),
        }];

        match self.generate(&test_message).await {
            Ok(response) => {
                // Check if the response status is successful
                if response.status().is_success() {
                    Ok(true)
                } else {
                    warn!("LLM service returned unsuccessful status: {}", response.status());
                    Ok(false)
                }
            },
            Err(e) => {
                warn!("Failed to connect to LLM service: {}", e);
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelConfig;
    use std::fs;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Config {
        deepseek_api_key: String,
    }

    fn load_config() -> Result<Config> {
        let config_str = fs::read_to_string("config.json")
            .context("Failed to read config.json")?;
        let config: Config = serde_json::from_str(&config_str)
            .context("Failed to parse config.json")?;
        Ok(config)
    }

    #[tokio::test]
    async fn test_basic_completion() -> Result<()> {
        let client = LlamaClient::new(ModelConfig {
            model: "phi4".to_string(),
            api_url: "http://localhost:11434/api".to_string(),
            stream: None,
            temperature: None,
            max_tokens: None,
            api_key: None,
        })?;
        let response = client.generate("What is Rust?").await?;
        assert!(!response.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_model() {
        let client = LlamaClient::new(ModelConfig {
            model: "non_existent_model".to_string(),
            api_url: "http://localhost:11434/api".to_string(),
            stream: None,
            temperature: None,
            max_tokens: None,
            api_key: None,
        }).unwrap();
        let result = client.generate("Test prompt").await;
        assert!(matches!(result.unwrap_err().downcast_ref(),
            Some(LlamaError::ModelNotAvailable(_, _))));
    }

    #[tokio::test]
    async fn test_service_unavailable() {
        let client = LlamaClient::new(ModelConfig {
            model: "llama2".to_string(),
            api_url: "http://localhost:11111/api".to_string(),
            stream: None,
            temperature: None,
            max_tokens: None,
            api_key: None,
        }).unwrap();
        let result = client.generate("Test prompt").await;
        assert!(matches!(result.unwrap_err().downcast_ref(),
            Some(LlamaError::ServiceUnavailable(_))));
    }

    #[tokio::test]
    async fn test_deep_seek() -> Result<()> {
        let client = LlamaClient::new(ModelConfig {
            model: "deepseek-chat".to_string(),
            api_url: "https://api.deepseek.com/v1/chat/completions".to_string(),
            stream: Some(false),
            temperature: Some(0.7),
            max_tokens: Some(2048),
            api_key: Some(std::env::var("DEEPSEEK_API_KEY")
                .context("DEEPSEEK_API_KEY environment variable not set")?),
        })?;
        
        let response = client.generate("Write a hello world in Rust").await?;
        assert!(!response.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_deepseek_completion() -> Result<()> {
        let config = load_config()?;
        
        let client = LlamaClient::new(ModelConfig {
            model: "deepseek-chat".to_string(),
            api_url: "https://api.deepseek.com/v1/chat/completions".to_string(),
            stream: Some(false),
            temperature: Some(0.7),
            max_tokens: Some(2048),
            api_key: Some(config.deepseek_api_key),
        })?;

        let response = client.generate("Write a hello world program in Rust").await?;
        assert!(!response.is_empty());
        println!("Deepseek response: {}", response);
        Ok(())
    }
} 
