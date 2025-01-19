use anyhow::{Result, Context};
use std::fs;
use reqwest::Client;
use serde_json::json;
use crate::config::ModelConfig;

#[derive(Debug, Clone)]
pub struct LlamaFunction {
    name: String,
    gbnf_file: String,
    prompt: String,
    llama_client: LlamaClient,
}

impl LlamaFunction {
    pub fn new(name: &str, gbnf_file: &str, prompt: &str, llama_client: LlamaClient) -> Self {
        Self {
            name: name.to_string(),
            gbnf_file: gbnf_file.to_string(),
            prompt: prompt.to_string(),
            llama_client,
        }
    }

    pub async fn call(&self) -> Result<String> {
        // Load the GBNF grammar
        let grammar = fs::read_to_string(&self.gbnf_file)
            .with_context(|| format!("Failed to read GBNF file: {}", self.gbnf_file))?;

        // Call the llama model using the local query_llama_cpp function
        let response_text = query_llama_cpp(
            &self.llama_client.config, // Assuming LlamaClient has a config field
            &self.prompt,
            &grammar
        ).await?;

        Ok(response_text)
    }
}

pub fn extract_nodes(html: &str) -> Result<Vec<Node>> {
    // Define the prompt and grammar directly
    let prompt = "Extract relevant information from the HTML content and format it as a JSON list of nodes.";
    let grammar = fs::read_to_string("gbnf/node_extract.gbnf")
        .with_context(|| "Failed to read GBNF file: gbnf/node_extract.gbnf")?;

    // Call the local query_llama_cpp function directly
    let response = query_llama_cpp(
        &llama_client.config, // Assuming llama_client is available in this context
        prompt,
        &grammar
    ).await?;

    Ok(response)
}

pub async fn query_llama_cpp(config: &ModelConfig, prompt: &str, grammar: &str) -> Result<String> {
    let client = Client::new();
    let url = config.llama_function_url.clone();

    let request_body = json!({
        "prompt": prompt,
        "grammar": grammar,
        "temperature": config.temperature.unwrap_or(0.7),
        "max_tokens": config.max_tokens.unwrap_or(2000),
    });

    let response = client.post(&url)
        .json(&request_body)
        .send()
        .await
        .context("Failed to send request to Llama.cpp server")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("Request failed with status {}: {}", status, body));
    }

    let response_text = response.text().await?;
    Ok(response_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelConfig;
    use tokio_test::block_on;

    #[tokio::test]
    async fn test_query_llama_cpp() {
        // Mock configuration
        let config = ModelConfig {
            llama_function_url: "http://localhost:8000".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(100),
            // Add other necessary fields if any
        };

        // Simple prompt
        let prompt = "What is the capital of France?";

        // GBNF grammar
        let grammar = r#"
        <response> ::= "Paris"
        "#;

        // Call the function
        let result = query_llama_cpp(&config, prompt, grammar).await;

        // Assert that the result is Ok
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
        
        // Optionally, check the content of the response
        if let Ok(response) = result {
            assert!(!response.is_empty(), "Response should not be empty");
        }
    }
}

