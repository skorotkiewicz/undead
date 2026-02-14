use anyhow::{Result, anyhow, bail};
use clap::Parser;
use colored::Colorize;
use dialoguer::{Input, theme::ColorfulTheme};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::PathBuf;

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

    /// Workspace directory for file operations (enables file tools)
    #[arg(short, long, value_name = "PATH")]
    workspace: Option<PathBuf>,
}

#[derive(Serialize, Debug)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Tool>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Message {
    role: String,
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: FunctionCall,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[derive(Serialize, Debug, Clone)]
struct Tool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ToolFunction,
}

#[derive(Serialize, Debug, Clone)]
struct ToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

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
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Deserialize, Debug)]
struct DeltaToolCall {
    id: Option<String>,
    #[serde(rename = "type")]
    tool_type: Option<String>,
    function: Option<DeltaFunction>,
}

#[derive(Deserialize, Debug)]
struct DeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

struct ChatApp {
    client: Client,
    args: Args,
    history: Vec<Message>,
    tools: Option<Vec<Tool>>,
    workspace: Option<PathBuf>,
}

impl ChatApp {
    fn new(args: Args) -> Self {
        let workspace = args.workspace.clone();
        let tools = workspace.as_ref().map(|_| Self::build_tools());

        Self {
            client: Client::new(),
            args,
            history: Vec::new(),
            tools,
            workspace,
        }
    }

    fn build_tools() -> Vec<Tool> {
        vec![
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    description: "Read the contents of a file from the workspace".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path to the file within the workspace"
                            }
                        },
                        "required": ["path"]
                    }),
                },
            },
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "write_file".to_string(),
                    description: "Write content to a file in the workspace. Creates the file if it doesn't exist, overwrites if it does.".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path to the file within the workspace"
                            },
                            "content": {
                                "type": "string",
                                "description": "Content to write to the file"
                            }
                        },
                        "required": ["path", "content"]
                    }),
                },
            },
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "create_directory".to_string(),
                    description: "Create a new directory in the workspace. Creates parent directories if needed.".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path to the directory within the workspace"
                            }
                        },
                        "required": ["path"]
                    }),
                },
            },
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "delete_file".to_string(),
                    description: "Delete a file from the workspace".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path to the file within the workspace"
                            }
                        },
                        "required": ["path"]
                    }),
                },
            },
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "delete_directory".to_string(),
                    description: "Delete a directory from the workspace. Directory must be empty.".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path to the directory within the workspace"
                            }
                        },
                        "required": ["path"]
                    }),
                },
            },
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "list_directory".to_string(),
                    description: "List the contents of a directory in the workspace".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path to the directory within the workspace (use '.' for workspace root)"
                            }
                        },
                        "required": ["path"]
                    }),
                },
            },
        ]
    }

    fn validate_path(&self, relative_path: &str) -> Result<PathBuf> {
        let workspace = self
            .workspace
            .as_ref()
            .ok_or_else(|| anyhow!("Workspace not configured"))?;

        let full_path = workspace.join(relative_path);

        // Canonicalize both paths to prevent directory traversal
        let canonical_workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.clone());

        // For paths that don't exist yet, we need to check the parent
        let canonical_path = if full_path.exists() {
            full_path
                .canonicalize()
                .map_err(|e| anyhow!("Failed to resolve path: {}", e))?
        } else {
            // Check parent directory exists and is within workspace
            let parent = full_path.parent().ok_or_else(|| anyhow!("Invalid path"))?;

            if parent.exists() {
                let canonical_parent = parent
                    .canonicalize()
                    .map_err(|e| anyhow!("Failed to resolve parent path: {}", e))?;

                if !canonical_parent.starts_with(&canonical_workspace) {
                    bail!("Path is outside workspace");
                }
            }
            full_path.clone()
        };

        if !canonical_path.starts_with(&canonical_workspace) {
            bail!("Path is outside workspace");
        }

        Ok(full_path)
    }

    async fn execute_tool(&self, name: &str, arguments: &str) -> Result<String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| anyhow!("Invalid tool arguments: {}", e))?;

        match name {
            "read_file" => {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing path argument"))?;
                let full_path = self.validate_path(path)?;

                let content = tokio::fs::read_to_string(&full_path)
                    .await
                    .map_err(|e| anyhow!("Failed to read file {}: {}", path, e))?;

                Ok(format!("File contents of {}:\n\n{}", path, content))
            }
            "write_file" => {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing path argument"))?;
                let content = args["content"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing content argument"))?;
                let full_path = self.validate_path(path)?;

                // Create parent directories if needed
                if let Some(parent) = full_path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|e| anyhow!("Failed to create parent directories: {}", e))?;
                }

                tokio::fs::write(&full_path, content)
                    .await
                    .map_err(|e| anyhow!("Failed to write file {}: {}", path, e))?;

                Ok(format!(
                    "Successfully wrote {} bytes to {}",
                    content.len(),
                    path
                ))
            }
            "create_directory" => {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing path argument"))?;
                let full_path = self.validate_path(path)?;

                tokio::fs::create_dir_all(&full_path)
                    .await
                    .map_err(|e| anyhow!("Failed to create directory {}: {}", path, e))?;

                Ok(format!("Successfully created directory {}", path))
            }
            "delete_file" => {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing path argument"))?;
                let full_path = self.validate_path(path)?;

                tokio::fs::remove_file(&full_path)
                    .await
                    .map_err(|e| anyhow!("Failed to delete file {}: {}", path, e))?;

                Ok(format!("Successfully deleted file {}", path))
            }
            "delete_directory" => {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing path argument"))?;
                let full_path = self.validate_path(path)?;

                tokio::fs::remove_dir(&full_path)
                    .await
                    .map_err(|e| anyhow!("Failed to delete directory {}: {}", path, e))?;

                Ok(format!("Successfully deleted directory {}", path))
            }
            "list_directory" => {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing path argument"))?;
                let full_path = self.validate_path(path)?;

                let mut entries = tokio::fs::read_dir(&full_path)
                    .await
                    .map_err(|e| anyhow!("Failed to read directory {}: {}", path, e))?;

                let mut result = format!("Contents of {}:\n", path);
                let mut files = Vec::new();
                let mut dirs = Vec::new();

                while let Some(entry) = entries.next_entry().await? {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if entry.file_type().await?.is_dir() {
                        dirs.push(format!("📁 {}/", name));
                    } else {
                        files.push(format!("📄 {}", name));
                    }
                }

                dirs.sort();
                files.sort();

                if dirs.is_empty() && files.is_empty() {
                    result.push_str("  (empty directory)\n");
                } else {
                    for d in dirs {
                        result.push_str(&format!("  {}\n", d));
                    }
                    for f in files {
                        result.push_str(&format!("  {}\n", f));
                    }
                }

                Ok(result)
            }
            _ => Err(anyhow!("Unknown tool: {}", name)),
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
            content: Some(user_input.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        loop {
            // Build request with history
            let mut messages = vec![Message {
                role: "system".to_string(),
                content: Some(self.args.system.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }];
            messages.extend(self.history.clone());

            let request = ChatRequest {
                model: self.args.model.clone(),
                messages,
                temperature: self.args.temperature,
                max_tokens: self.args.max_tokens,
                stream: true,
                tools: self.tools.clone(),
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
            let mut tool_calls: Vec<ToolCall> = Vec::new();
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
                                // Handle content
                                if let Some(content) = &choice.delta.content {
                                    print!("{}", content);
                                    io::stdout().flush()?;
                                    full_response.push_str(content);
                                }

                                // Handle tool calls
                                if let Some(delta_tool_calls) = &choice.delta.tool_calls {
                                    for delta_tc in delta_tool_calls {
                                        // Find or create tool call
                                        if let Some(id) = &delta_tc.id {
                                            // New tool call
                                            tool_calls.push(ToolCall {
                                                id: id.clone(),
                                                tool_type: delta_tc
                                                    .tool_type
                                                    .clone()
                                                    .unwrap_or_else(|| "function".to_string()),
                                                function: FunctionCall {
                                                    name: delta_tc
                                                        .function
                                                        .as_ref()
                                                        .and_then(|f| f.name.clone())
                                                        .unwrap_or_default(),
                                                    arguments: delta_tc
                                                        .function
                                                        .as_ref()
                                                        .and_then(|f| f.arguments.clone())
                                                        .unwrap_or_default(),
                                                },
                                            });
                                        } else if let Some(last_tc) = tool_calls.last_mut() {
                                            // Append to existing tool call
                                            if let Some(args) = &delta_tc
                                                .function
                                                .as_ref()
                                                .and_then(|f| f.arguments.as_ref())
                                            {
                                                last_tc.function.arguments.push_str(args);
                                            }
                                        }
                                    }
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

            // If there are tool calls, execute them and continue the conversation
            if !tool_calls.is_empty() {
                println!("{}", "Executing tools...".yellow().bold());

                // Add assistant message with tool calls to history
                self.history.push(Message {
                    role: "assistant".to_string(),
                    content: if full_response.is_empty() {
                        None
                    } else {
                        Some(full_response.clone())
                    },
                    tool_calls: Some(tool_calls.clone()),
                    tool_call_id: None,
                    name: None,
                });

                // Execute each tool and add results to history
                for tc in &tool_calls {
                    println!("  {} {}", "→".cyan(), tc.function.name.bright_cyan());

                    let result = match self
                        .execute_tool(&tc.function.name, &tc.function.arguments)
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => format!("Error: {}", e),
                    };

                    println!("    {}", result.lines().next().unwrap_or(&result).dimmed());

                    self.history.push(Message {
                        role: "tool".to_string(),
                        content: Some(result),
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                        name: Some(tc.function.name.clone()),
                    });
                }

                println!("{}", "Processing results...".yellow().bold());
                print!("{} ", "Assistant:".green().bold());
                io::stdout().flush()?;

                // Continue the loop to get the next response
                continue;
            }

            // Add assistant response to history
            self.history.push(Message {
                role: "assistant".to_string(),
                content: Some(full_response.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });

            return Ok(full_response);
        }
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

        if let Some(ref workspace) = self.workspace {
            println!(
                "  {} {}",
                "Workspace:".dimmed(),
                workspace.display().to_string().dimmed()
            );
            println!("  {} {}", "File tools:".dimmed(), "enabled".green());
        }

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
