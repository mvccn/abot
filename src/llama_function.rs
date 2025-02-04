use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;
use std::fs;

#[derive(Debug, Clone)]
pub struct LlamaFunction {
    name: String,
    gbnf_file: Option<String>,
    prompt: String,
    client: Client,
}

impl LlamaFunction {
    pub fn new(name: &str, gbnf_file: Option<&str>, prompt: &str) -> Self {
        Self {
            name: name.to_string(),
            gbnf_file: gbnf_file.map(|s| s.to_string()),
            prompt: prompt.to_string(),
            client: Client::new(),
        }
    }

    async fn call(&self) -> Result<String> {
        // Load the GBNF grammar if provided
        let grammar = if let Some(ref gbnf_file) = self.gbnf_file {
            Some(fs::read_to_string(gbnf_file)
                .with_context(|| format!("Failed to read GBNF file: {}", gbnf_file))?)
        } else {
            None
        };

        // Call the llama model using the local query_llama_cpp function
        let response_text =self.query_llama_with_grammar(&self.prompt, grammar.as_deref()).await?;

        Ok(response_text)
    }

    async fn query_llama_with_grammar(&self, prompt: &str, grammar: Option<&str>) -> Result<String> {
        let url = "http://localhost:9000/completion".to_string();

        let request_body = json!({
            "prompt": prompt,
            "n_predict": 128,
            // "grammar": grammar.unwrap_or(""),
            // "temperature": config.temperature.unwrap_or(0.7),
            // "max_tokens": config.max_tokens.unwrap_or(2000),
        });

        let response = self.client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to send request to Llama.cpp server")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Request failed with status {}: {}",
                status,
                body
            ));
        }

        let response_text = response.text().await?;
        
        // Parse the JSON response and extract the "content" field
        let response_json: serde_json::Value = serde_json::from_str(&response_text)
            .context("Failed to parse response JSON")?;
        let content = response_json["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' field in response"))?
            .to_string();

        Ok(content)
    }

    pub async fn extract_nodes(&self, query: &str, html: &str) -> Result<String> {
        // Load the JSON GBNF grammar
        // let grammar = fs::read_to_string("src/gbnf/json.gbnf")
        //     .context("Failed to read json.gbnf file")?;
        let grammar = r#"
        root   ::= object
        value  ::= object | array | string | number | ("true" | "false" | "null") ws
        
        object ::=
          "{" ws (
                    string ":" ws value
            ("," ws string ":" ws value)*
          )? "}" ws
        
        array  ::=
          "[" ws (
                    value
            ("," ws value)*
          )? "]" ws
        
        string ::=
          "\"" (
            [^"\\\x7F\x00-\x1F] |
            "\\" (["\\bfnrt] | "u" [0-9a-fA-F]{4}) # escapes
          )* "\"" ws
        
        number ::= ("-"? ([0-9] | [1-9] [0-9]{0,15})) ("." [0-9]+)? ([eE] [-+]? [0-9] [1-9]{0,15})? ws
        
        # Optional space: by convention, applied in this grammar after literal chars when allowed
        ws ::= | " " | "\n" [ \t]{0,20}
        "#;

        // Define the prompt directly
        let prompt = format!(
            r#"You are a web content analyzer. Please analyze the following HTML document and extract key information into a structured JSON format. Focus on:

1. Main topic or title
2. Key concepts and ideas
3. Important facts and data
4. Relevant dates and timestamps
5. Author information (if available)
6. Links and references
7. Any content specifically related to: "{}"

Format the response as a JSON object with these categories. If any category is not found, set it to null.
Here's the document to analyze:

{}"#,
            query, html
        );

        // Call the Llama service with the JSON grammar
        let response_text = self.query_llama_with_grammar(&prompt, Some(&grammar)).await?;

        // Return the raw JSON response
        Ok(response_text)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_search::fetch_url;
    use anyhow::Result;
    
    #[tokio::test]
    async fn test_web_content_extraction() -> Result<()> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; abot-test/1.0)")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        // Test a known simple page
        let url = "https://example.com/";
        let content = fetch_url(&client, url).await?;
        assert!(!content.is_empty(), "Should fetch page content");

        let llama_function = LlamaFunction::new("web_extract", None, "");
        let query = "main programming features";
        
        // Extract nodes from the raw HTML
        let result = llama_function.extract_nodes(query, &content).await?;
        println!("Extraction result: {}", result);
        
        // Verify we got structured JSON output
        assert!(result.starts_with('{') || result.starts_with('['));
        assert!(result.len() > 50, "Should get meaningful extraction");
        
        // Basic sanity checks on the JSON
        let json_val: serde_json::Value = serde_json::from_str(&result)?;
        assert!(!json_val.is_null(), "Should parse valid JSON");

        Ok(())
    }

    #[tokio::test]
    async fn test_llama_server_connection() {
        let llama_function = LlamaFunction::new("test_connection", None, "");
        let result = llama_function
            .query_llama_with_grammar("What is the capital of France?", None)
            .await;

        match result {
            Ok(response) => {
                println!("Response: {}", response);
                assert!(!response.is_empty(), "Response should not be empty");
            }
            Err(e) => {
                println!("Error: {}", e);
                panic!("Test failed: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_fetch(){
        use crate::web_search::fetch_url;
        let client = Client::new();
        // For testing, let's try a more accessible URL first
        let url =  "https://wwww.google.com";   
        let content = fetch_url(&client, url).await.expect("Failed to fetch URL");
        assert!(!content.is_empty(), "Content should not be empty");
        println!("Content: {}", content);
    }

    // test local llama server
    // llama-server -m ~/models/phi-4-q4.gguf --port 9000 -np 1
    #[tokio::test]
    async fn test_llama_server_completion() -> Result<()> {
        let client = Client::new();
        let url = "http://localhost:9000/v1/chat/completions";
        
        let request_body = json!({
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, how are you?"
                }
            ],
            "temperature": 0.7
        });

        let response = client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to connect to Llama server")?;

        assert!(
            response.status().is_success(),
            "Server returned error status: {}",
            response.status()
        );

        let response_json: serde_json::Value = response.json().await
            .context("Failed to parse response as JSON")?;
        
        // Print the full response for debugging
        println!("Response: {}", serde_json::to_string_pretty(&response_json)?);

        // Basic validation of response structure
        assert!(response_json.get("choices").is_some(), "Response should contain 'choices' field");
        
        Ok(())
    }

    #[tokio::test]
    async fn test_query_llama_cpp_with_json_grammar() {
        // Create an instance of LlamaFunction
        let llama_function = LlamaFunction::new("test", None, "");

        // Simple prompt
        let prompt = "What is the capital of France? Please format the response as a JSON object. ";

        // Use the JSON GBNF grammar
        let grammar = r#"
        <json> ::= <node_list>
        <node_list> ::= "[" <node> ("," <node>)* "]"
        <node> ::= "{" <node_name> "," <node_info> "}"
        <node_name> ::= "\"name\": \"" <string> "\""
        <node_info> ::= "\"info\": \"" <string> "\""
        <string> ::= <character>*
        <character> ::= <letter> | <digit> | " " | "," | "." | "-" | "_"
        <letter> ::= "a" | "b" | "c" | ... | "z" | "A" | "B" | ... | "Z"
        <digit> ::= "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
        "#;

        // Call the function
        let result = llama_function.query_llama_with_grammar(prompt, Some(grammar)).await;

        // Assert that the result is Ok
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
        println!("Response: {}", result.unwrap());
    }

}
