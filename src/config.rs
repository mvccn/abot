use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use log::info;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WebSearchConfig {
    pub result_limit: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub default: DefaultConfig,
    pub default_provider: String,
    pub deepseek: ModelConfig,
    pub openai: ModelConfig,
    pub llamacpp: ModelConfig,
    pub ollama: ModelConfig,
    pub web_search: WebSearchConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DefaultConfig {
    pub temperature: f32,
    pub max_tokens: u32,
    pub stream: bool,
    pub initial_prompt: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default: DefaultConfig {
                temperature: 0.7,
                max_tokens: 2000,
                stream: true,
                initial_prompt: String::from("You are a helpful AI assistant."),
            },
            default_provider: String::from("llamacpp"),
            deepseek: ModelConfig {
                api_url: String::from("https://api.deepseek.com/v1/chat/completions"),
                api_key: Some(String::from("your-deepseek-key")),
                model: String::from("deepseek-chat"),
                temperature: None,
                max_tokens: None,
                stream: None,
            },
            openai: ModelConfig {
                api_url: String::from("https://api.openai.com/v1/chat/completions"),
                api_key: Some(String::from("your-openai-key")),
                model: String::from("gpt-3.5-turbo"),
                temperature: None,
                max_tokens: None,
                stream: None,
            },
            llamacpp: ModelConfig {
                api_url: String::from("http://localhost:8080/v1/chat/completions"),
                api_key: None,
                model: String::from("phi4"),
                temperature: None,
                max_tokens: None,
                stream: None,
            },
            ollama: ModelConfig {
                api_url: String::from("http://localhost:11434/api/chat"),
                api_key: None,
                model: String::from("mistral"),
                temperature: None,
                max_tokens: None,
                stream: None,
            },
            web_search: WebSearchConfig { result_limit: 10 },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelConfig {
    pub api_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stream: Option<bool>,
}

impl ModelConfig {
    pub fn get_temperature(&self, defaults: &DefaultConfig) -> f32 {
        self.temperature.unwrap_or(defaults.temperature)
    }

    pub fn get_max_tokens(&self, defaults: &DefaultConfig) -> u32 {
        self.max_tokens.unwrap_or(defaults.max_tokens)
    }

    pub fn get_stream(&self, defaults: &DefaultConfig) -> bool {
        self.stream.unwrap_or(defaults.stream)
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            api_url: String::new(),
            api_key: None,
            model: String::new(),
            temperature: None,
            max_tokens: None,
            stream: None,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .join(".config")
            .join("abot");

        let config_path = config_dir.join("config.toml");

        if !config_dir.exists() {
            info!("Creating config directory: {}", config_dir.display());
            fs::create_dir_all(&config_dir)?;
        }

        if !config_path.exists() {
            info!("Creating default config file: {}", config_path.display());
            let default_config = Config::default();
            let toml = toml::to_string_pretty(&default_config)?;
            fs::write(&config_path, toml)?;
            info!("Please set your API key in the config file or DEEPSEEK_API_KEY environment variable");
            info!("You can edit the config file at: {}", config_path.display());
            return Ok(default_config);
        }

        info!("Loading config from: {}", config_path.display());
        let config_str = fs::read_to_string(&config_path)?;
        let config: Config = toml::from_str(&config_str)?;

        if config.deepseek.api_key.is_none() && std::env::var("DEEPSEEK_API_KEY").is_err() {
            warn!(
                "No API key found in config file or DEEPSEEK_API_KEY environment variable"
            );
            info!("Please set your API key in: {}", config_path.display());
            info!("Or set the DEEPSEEK_API_KEY environment variable");
        }

        Ok(config)
    }
}
