use std::{fs, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use futures_util::StreamExt;
use orbit_model::{
    client::{ApiFormat, ModelClient},
    types::{ChatMessage, ChatRequest, ModelConfig, StreamEvent},
};
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(name = "orbit")]
struct Cli {
    #[arg(
        long,
        env = "orbit_config",
        default_value = "config.toml",
        global = true
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Chat(ChatArgs),
}

#[derive(Debug, Args)]
struct ChatArgs {
    #[arg(long, short)]
    stream: bool,

    prompt: String,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default = "default_api_format")]
    api_format: ApiFormat,
    base_url: String,
    api_key: String,
    model: String,
    timeout_seconds: u64,
}

fn default_api_format() -> ApiFormat {
    ApiFormat::OpenaiChatCompletions
}

impl FileConfig {
    fn load(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        if config.api_key.trim().is_empty() {
            bail!("api_key is empty in {}", path.display());
        }

        Ok(config)
    }

    fn model_config(self) -> ModelConfig {
        ModelConfig {
            api_format: self.api_format,
            base_url: self.base_url,
            api_key: Some(self.api_key),
            model: self.model,
            timeout: Duration::from_secs(self.timeout_seconds),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Chat(args) => chat(cli.config, args).await,
    }
}

async fn chat(config_path: PathBuf, args: ChatArgs) -> Result<()> {
    let file_config = FileConfig::load(&config_path)?;
    let client = ModelClient::new(file_config.model_config())?;
    let request = ChatRequest::new(vec![ChatMessage::user(args.prompt)]);

    if args.stream {
        let mut events = client.stream(request).await?;

        while let Some(event) = events.next().await {
            match event? {
                StreamEvent::TextDelta(text) | StreamEvent::ReasoningDelta(text) => {
                    print!("{text}");
                }
                StreamEvent::Completed { .. } | StreamEvent::ToolCallDelta(_) => {}
            }
        }

        println!();
    } else {
        let completion = client.complete(request).await?;

        if let orbit_model::types::ChatMessage::Assistant { content, .. } = completion.message {
            println!("{}", content.unwrap_or_default());
        }
    }

    Ok(())
}
