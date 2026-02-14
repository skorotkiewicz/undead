use anyhow::{Result, anyhow};
use clap::Parser;
use colored::Colorize;
use dialoguer::{Input, theme::ColorfulTheme};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

/// A simple CLI chat application for OpenAI-compatible APIs
#[derive(Parser, Debug)]
#[command(name = "undead")]
#[command(about = "Chat with OpenAI-compatible LLM APIs", long_about = None)]
struct Args {
    /// API base endpoint
    #[arg(short, long, default_value = "http://localhost:8080/v1")]
    endpoint: String,

    /// Model to use for chat
    #[arg(short, long, default_value = "local-model")]
    model: String,

    /// API key (optional)
    #[arg(short, long, default_value = "dummy-key")]
    api_key: String,

    /// System prompt to set the assistant's behavior
    #[arg(short, long, default_value = "You are a helpful assistant.")]
    system: String,

    /// Temperature for response randomness (0.0 - 2.0)
    #[arg(short, long, default_value = "0.7")]
    temperature: f32,

    /// Maximum tokens in the response
    #[arg(short = 't', long, default_value = "2048")]
    max_tokens: u32,
}

#[derive(Serialize, Debug)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Message {
    role: String,
    content: String,
}

// #[derive(Deserialize, Debug)]
// struct ChatResponse {
//     choices: Vec<Choice>,
// }

// #[derive(Deserialize, Debug)]
// struct Choice {
//     message: Message,
// }

#[derive(Deserialize, Debug)]
struct StreamResponse {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize, Debug)]
struct StreamChoice {
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct Delta {
    content: Option<String>,
}

struct ChatApp {
    client: Client,
    args: Args,
    history: Vec<Message>,
}

impl ChatApp {
    fn new(args: Args) -> Self {
        Self {
            client: Client::new(),
            args,
            history: Vec::new(),
        }
    }

    fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        if !self.args.api_key.is_empty() {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.args.api_key).parse().unwrap(),
            );
        }
        headers
    }

    async fn send_message(&mut self, user_input: &str) -> Result<String> {
        // Add user message to history
        self.history.push(Message {
            role: "user".to_string(),
            content: user_input.to_string(),
        });

        // Build request with history
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: self.args.system.clone(),
        }];
        messages.extend(self.history.clone());

        let request = ChatRequest {
            model: self.args.model.clone(),
            messages,
            temperature: self.args.temperature,
            max_tokens: self.args.max_tokens,
            stream: true,
        };

        let endpoint = format!("{}/chat/completions", self.args.endpoint);
        let response = self
            .client
            .post(&endpoint)
            .headers(self.build_headers())
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(anyhow!("API error: {}", error_text));
        }

        // Process streaming response
        let mut full_response = String::new();
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            // Process complete SSE messages
            while let Some(pos) = buffer.find("\n\n") {
                let message = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                if message.starts_with("data: ") {
                    let data = &message[6..];
                    if data == "[DONE]" {
                        break;
                    }

                    if let Ok(stream_response) = serde_json::from_str::<StreamResponse>(data) {
                        if let Some(choice) = stream_response.choices.first() {
                            if let Some(content) = &choice.delta.content {
                                print!("{}", content);
                                io::stdout().flush()?;
                                full_response.push_str(content);
                            }
                            if choice.finish_reason.is_some() {
                                break;
                            }
                        }
                    }
                }
            }
        }

        println!(); // New line after response

        // Add assistant response to history
        self.history.push(Message {
            role: "assistant".to_string(),
            content: full_response.clone(),
        });

        Ok(full_response)
    }

    async fn run(&mut self) -> Result<()> {
        println!(
            "{}",
            "╔════════════════════════════════════════════╗".cyan()
        );
        println!(
            "{}",
            "║     🧟 Undead - LLM Chat Client            ║".cyan()
        );
        println!(
            "{}",
            "╚════════════════════════════════════════════╝".cyan()
        );
        println!();
        println!(
            "  {} {}",
            "API endpoint:".dimmed(),
            self.args.endpoint.dimmed()
        );
        println!("  {} {}", "Model:".dimmed(), self.args.model.dimmed());
        println!();
        println!("{}", "Type your message and press Enter to chat.".dimmed());
        println!(
            "{}",
            "Type 'exit', 'quit', or press Ctrl+C to exit.".dimmed()
        );
        println!("{}", "Type 'clear' to clear conversation history.".dimmed());
        println!();

        loop {
            let input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("You")
                .interact_text()?;

            let trimmed = input.trim();

            if trimmed.is_empty() {
                continue;
            }

            match trimmed.to_lowercase().as_str() {
                "exit" | "quit" | "q" => {
                    println!("\n{}", "Goodbye! 👋".green());
                    break;
                }
                "clear" => {
                    self.history.clear();
                    println!("{}", "Conversation history cleared.".yellow());
                    continue;
                }
                _ => {}
            }

            print!("{} ", "Assistant:".green().bold());
            io::stdout().flush()?;

            if let Err(e) = self.send_message(trimmed).await {
                eprintln!("\n{} {}", "Error:".red(), e);
            }
            println!();
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut app = ChatApp::new(args);
    app.run().await
}
