use anyhow::{Result, anyhow, bail};
use clap::Parser;
use colored::Colorize;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;
use futures_util::StreamExt;
use reqwest::Client;
use rmcp::model::Tool as McpTool;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransport;
use rmcp::{Peer, RoleClient, Service, serve_client};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A simple CLI chat application for OpenAI-compatible APIs
#[derive(Parser, Debug)]
#[command(name = "undead")]
#[command(about = "Chat with OpenAI-compatible LLM APIs", long_about = None)]
struct Args {
    /// API base endpoint
    #[arg(
        short,
        long,
        default_value = "http://localhost:8080/v1",
        env = "UNDEAD_ENDPOINT"
    )]
    endpoint: String,

    /// Model to use for chat
    #[arg(short, long, default_value = "local-model", env = "UNDEAD_MODEL")]
    model: String,

    /// API key (optional)
    #[arg(short, long, default_value = "dummy-key", env = "UNDEAD_API_KEY")]
    api_key: String,

    /// System prompt to set the assistant's behavior
    #[arg(
        short,
        long,
        default_value = "You are a helpful assistant.",
        env = "UNDEAD_SYSTEM"
    )]
    system: String,

    /// Temperature for response randomness (0.0 - 2.0)
    #[arg(short, long, default_value = "0.7", env = "UNDEAD_TEMPERATURE")]
    temperature: f32,

    /// Maximum tokens in the response
    #[arg(short = 'T', long, default_value = "2048", env = "UNDEAD_MAX_TOKENS")]
    max_tokens: u32,

    /// Workspace directory for file operations (enables file tools)
    #[arg(short, long, value_name = "PATH", env = "UNDEAD_WORKSPACE")]
    workspace: Option<PathBuf>,

    /// MCP configuration file path (enables MCP tools)
    #[arg(short = 'c', long, value_name = "PATH", env = "UNDEAD_MCP")]
    mcp: Option<PathBuf>,

    /// Configuration file path
    #[arg(short = 'C', long, value_name = "PATH", env = "UNDEAD_CONFIG")]
    config: Option<PathBuf>,

    /// Preset name from config file
    #[arg(short = 'p', long, env = "UNDEAD_PRESET")]
    preset: Option<String>,

    /// Version
    #[arg(short = 'V', long)]
    version: bool,
}

#[derive(Deserialize, Debug)]
struct ConfigFile {
    #[serde(flatten)]
    global: HashMap<String, String>,
    presets: HashMap<String, HashMap<String, String>>,
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

#[derive(Deserialize, Debug)]
struct McpConfig {
    mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Deserialize, Debug)]
struct McpServerConfig {
    #[serde(rename = "type")]
    server_type: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
    url: Option<String>,
    enabled: Option<bool>,
}

struct McpClient {
    name: String,
    peer: Peer<RoleClient>,
    tools: Vec<McpTool>,
    #[allow(dead_code)]
    service: RunningService<RoleClient, ClientHandler>,
}

// Simple client handler for MCP
struct ClientHandler;

impl Service<RoleClient> for ClientHandler {
    async fn handle_request(
        &self,
        _request: rmcp::model::ServerRequest,
        _context: rmcp::service::RequestContext<RoleClient>,
    ) -> Result<rmcp::model::ClientResult, rmcp::ErrorData> {
        Ok(rmcp::model::ClientResult::empty(()))
    }

    async fn handle_notification(
        &self,
        _notification: rmcp::model::ServerNotification,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) -> Result<(), rmcp::ErrorData> {
        Ok(())
    }

    fn get_info(&self) -> rmcp::model::ClientInfo {
        rmcp::model::ClientInfo::default()
    }
}

struct AgentContext {
    files: Vec<(String, String)>, // (filename, content)
}

impl AgentContext {
    fn load(workspace: &Path) -> Self {
        let agent_dir = workspace.join(".agent");

        // Create .agent directory if it doesn't exist
        if !agent_dir.exists() {
            let _ = std::fs::create_dir_all(&agent_dir);
        }

        // Read all *.md files from .agent directory
        let mut files = Vec::new();
        let pattern = format!("{}/*.md", agent_dir.display());

        if let Ok(paths) = glob::glob(&pattern) {
            for entry in paths.filter_map(|e| e.ok()) {
                if entry.is_file() {
                    if let Some(filename) = entry.file_name() {
                        if let Some(name) = filename.to_str() {
                            if let Ok(content) = std::fs::read_to_string(&entry) {
                                files.push((name.to_string(), content));
                            }
                        }
                    }
                }
            }
        }

        // Sort by filename for consistent ordering
        files.sort_by(|a, b| a.0.cmp(&b.0));

        Self { files }
    }

    fn to_system_prompt(&self) -> String {
        self.files
            .iter()
            .map(|(name, content)| format!("# {}\n{}", name, content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn has_content(&self) -> bool {
        !self.files.is_empty()
    }
}

fn open_editor() -> Result<String> {
    // Create a temporary file
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("undead_input.md");

    // Create empty file if it doesn't exist
    std::fs::write(&temp_file, "")?;

    // Get editor from environment or use platform-specific default
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad".to_string()
            } else {
                "vi".to_string()
            }
        });

    // Open editor
    let status = Command::new(&editor)
        .arg(&temp_file)
        .status()
        .map_err(|e| anyhow!("Failed to open editor '{}': {}", editor, e))?;

    if !status.success() {
        return Err(anyhow!("Editor exited with non-zero status"));
    }

    // Read content from temp file
    let content = std::fs::read_to_string(&temp_file)?;

    // Clean up - remove temp file
    let _ = std::fs::remove_file(&temp_file);

    Ok(content.trim().to_string())
}

fn get_history_path(workspace: &Path) -> PathBuf {
    workspace.join(".agent").join("HISTORY.log")
}

fn load_history(workspace: &Path) -> Vec<Message> {
    let history_path = get_history_path(workspace);
    if !history_path.exists() {
        return Vec::new();
    }

    if let Ok(content) = std::fs::read_to_string(&history_path) {
        // Parse history log format: "--- ROLE ---\ncontent\n--- END ---"
        let mut messages = Vec::new();
        let parts: Vec<&str> = content.split("--- END ---").collect();

        for part in parts {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if part.starts_with("--- USER ---") {
                let content = part.strip_prefix("--- USER ---").unwrap_or("").trim();
                if !content.is_empty() {
                    messages.push(Message {
                        role: "user".to_string(),
                        content: Some(content.to_string()),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }
            } else if part.starts_with("--- ASSISTANT ---") {
                let content = part.strip_prefix("--- ASSISTANT ---").unwrap_or("").trim();
                if !content.is_empty() {
                    messages.push(Message {
                        role: "assistant".to_string(),
                        content: Some(content.to_string()),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }
            }
        }

        messages
    } else {
        Vec::new()
    }
}

fn save_history(workspace: &Path, messages: &[Message]) {
    let history_path = get_history_path(workspace);

    // Create .agent directory if it doesn't exist
    if let Some(parent) = history_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut content = String::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" => {
                content.push_str(&format!(
                    "--- USER ---\n{}\n--- END ---\n\n",
                    msg.content.as_deref().unwrap_or("")
                ));
            }
            "assistant" => {
                content.push_str(&format!(
                    "--- ASSISTANT ---\n{}\n--- END ---\n\n",
                    msg.content.as_deref().unwrap_or("")
                ));
            }
            _ => {}
        }
    }

    let _ = std::fs::write(&history_path, content);
}

struct ChatApp {
    client: Client,
    args: Args,
    history: Vec<Message>,
    tools: Option<Vec<Tool>>,
    workspace: Option<PathBuf>,
    mcp_clients: Arc<RwLock<Vec<McpClient>>>,
    agent_context: Option<AgentContext>,
}

impl McpClient {
    async fn connect_local(
        name: String,
        command: String,
        args: Option<Vec<String>>,
        env: Option<HashMap<String, String>>,
    ) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(&command);

        if let Some(args) = args {
            cmd.args(args);
        }

        if let Some(env_vars) = env {
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
        }

        let transport = TokioChildProcess::new(cmd)?;
        let service = ClientHandler;
        let running_service = serve_client(service, transport).await?;
        let peer = running_service.peer().clone();

        let tools_result = peer.list_tools(None).await?;
        let tools = tools_result.tools;

        Ok(Self {
            name,
            peer,
            tools,
            service: running_service,
        })
    }

    async fn connect_remote(name: String, url: String) -> Result<Self> {
        // Create HTTP transport for remote MCP server
        let transport = StreamableHttpClientTransport::from_uri(url);

        let service = ClientHandler;
        let running_service = serve_client(service, transport).await?;
        let peer = running_service.peer().clone();

        let tools_result = peer.list_tools(None).await?;
        let tools = tools_result.tools;

        Ok(Self {
            name,
            peer,
            tools,
            service: running_service,
        })
    }

    fn get_tools(&self) -> Vec<Tool> {
        self.tools
            .iter()
            .map(|tool| Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: format!("mcp_{}_{}", self.name, tool.name),
                    description: tool.description.clone().unwrap_or_default().to_string(),
                    parameters: serde_json::Value::Object(tool.input_schema.as_ref().clone()),
                },
            })
            .collect()
    }
}

impl ChatApp {
    fn new(args: Args) -> Self {
        let workspace = args.workspace.clone();
        let tools = workspace.as_ref().map(|_| Self::build_tools());
        let agent_context = workspace.as_ref().map(|w| AgentContext::load(w));

        // Load history from workspace/.agent/HISTORY.log
        let history = workspace
            .as_ref()
            .map(|w| load_history(w))
            .unwrap_or_default();

        Self {
            client: Client::new(),
            args,
            history,
            tools,
            workspace,
            mcp_clients: Arc::new(RwLock::new(Vec::new())),
            agent_context,
        }
    }

    async fn initialize_mcp(&mut self) -> Result<()> {
        if let Some(mcp_path) = &self.args.mcp {
            let config_content = std::fs::read_to_string(mcp_path)?;
            let config: McpConfig = serde_json::from_str(&config_content)?;

            let mut mcp_clients = self.mcp_clients.write().await;
            let mut all_tools = self.tools.take().unwrap_or_default();

            for (server_name, server_config) in &config.mcp_servers {
                let enabled = server_config.enabled.unwrap_or(true);
                if !enabled {
                    continue;
                }

                let server_type = server_config.server_type.as_deref().unwrap_or("local");

                println!("{} MCP server: {}", "Connecting to".cyan(), server_name);

                let mcp_client = match server_type {
                    "remote" => {
                        let url = server_config.url.as_ref().ok_or_else(|| {
                            anyhow!("Remote MCP server {} missing URL", server_name)
                        })?;
                        McpClient::connect_remote(server_name.clone(), url.clone()).await
                    }
                    _ => {
                        let command = server_config.command.as_ref().ok_or_else(|| {
                            anyhow!("Local MCP server {} missing command", server_name)
                        })?;
                        McpClient::connect_local(
                            server_name.clone(),
                            command.clone(),
                            server_config.args.clone(),
                            server_config.env.clone(),
                        )
                        .await
                    }
                };

                match mcp_client {
                    Ok(client) => {
                        println!(
                            "{} Connected to {} ({} tools)",
                            "✓".green(),
                            server_name,
                            client.tools.len()
                        );
                        all_tools.extend(client.get_tools());
                        mcp_clients.push(client);
                    }
                    Err(e) => {
                        eprintln!("{} Failed to connect to {}: {}", "✗".red(), server_name, e);
                    }
                }
            }

            self.tools = if all_tools.is_empty() {
                None
            } else {
                Some(all_tools)
            };
        }

        Ok(())
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
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "edit_file".to_string(),
                    description: "Edit a file by replacing a specific string with another string. Use this for precise edits where you know the exact text to replace.".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path to the file within the workspace"
                            },
                            "old_text": {
                                "type": "string",
                                "description": "The exact text to find and replace. Must match exactly including whitespace."
                            },
                            "new_text": {
                                "type": "string",
                                "description": "The text to replace the old_text with"
                            }
                        },
                        "required": ["path", "old_text", "new_text"]
                    }),
                },
            },
            Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "execute".to_string(),
                    description: "Execute a shell command in the workspace directory. Use with caution. The command runs in the workspace context.".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "command": {
                                "type": "string",
                                "description": "Shell command to execute (e.g., 'ls -la', 'cat file.txt | grep pattern')"
                            }
                        },
                        "required": ["command"]
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

        // Canonicalize workspace to get absolute path
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(|e| anyhow!("Workspace directory does not exist: {}", e))?;

        let full_path = workspace.join(relative_path);

        // For paths that don't exist yet, we need to check the parent
        if full_path.exists() {
            // Path exists - canonicalize and verify it's within workspace
            let canonical_path = full_path
                .canonicalize()
                .map_err(|e| anyhow!("Failed to resolve path: {}", e))?;

            if !canonical_path.starts_with(&canonical_workspace) {
                bail!("Path is outside workspace");
            }
        } else {
            // Path doesn't exist - check parent directory
            let mut current = full_path.parent();
            let mut found_valid_parent = false;

            // Walk up the tree to find an existing parent
            while let Some(parent) = current {
                if parent.exists() {
                    let canonical_parent = parent
                        .canonicalize()
                        .map_err(|e| anyhow!("Failed to resolve parent path: {}", e))?;

                    if !canonical_parent.starts_with(&canonical_workspace) {
                        bail!("Path is outside workspace");
                    }
                    found_valid_parent = true;
                    break;
                }
                current = parent.parent();
            }

            // If no parent exists, check if the path itself would be within workspace
            if !found_valid_parent {
                // This handles the case where we're creating files in the workspace root
                // or creating nested directories that don't exist yet
                let path_str = full_path.to_string_lossy();
                let workspace_str = canonical_workspace.to_string_lossy();

                if !path_str.starts_with(&*workspace_str) {
                    bail!("Path is outside workspace");
                }
            }
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
            "edit_file" => {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing path argument"))?;
                let old_text = args["old_text"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing old_text argument"))?;
                let new_text = args["new_text"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing new_text argument"))?;
                let full_path = self.validate_path(path)?;

                let content = tokio::fs::read_to_string(&full_path)
                    .await
                    .map_err(|e| anyhow!("Failed to read file {}: {}", path, e))?;

                if !content.contains(old_text) {
                    return Err(anyhow!(
                        "Could not find the text to replace in file {}. The old_text must match exactly.",
                        path
                    ));
                }

                let occurrences = content.matches(old_text).count();
                let new_content = content.replace(old_text, new_text);

                tokio::fs::write(&full_path, &new_content)
                    .await
                    .map_err(|e| anyhow!("Failed to write file {}: {}", path, e))?;

                Ok(format!(
                    "Successfully replaced {} occurrence(s) of old_text with new_text in {}",
                    occurrences, path
                ))
            }
            "execute" => {
                let command = args["command"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing command argument"))?;
                let workspace = self
                    .workspace
                    .as_ref()
                    .ok_or_else(|| anyhow!("Workspace not configured"))?;

                // Ask user for confirmation before executing
                println!(
                    "\n{} Execute command: {}",
                    "?".yellow().bold(),
                    command.cyan()
                );
                print!("Run? (y/N): ");
                io::stdout().flush()?;

                let mut confirmation = String::new();
                io::stdin().read_line(&mut confirmation)?;

                if confirmation.trim().to_lowercase() != "y" {
                    return Ok("Command execution cancelled by user".to_string());
                }

                println!("{}", "Press 'c' to cancel while executing...".dimmed());

                // Spawn the command with streaming output
                let mut child = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .current_dir(workspace)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| anyhow!("Failed to execute command: {}", e))?;

                let pid = child.id();
                let mut result = String::new();
                let mut cancelled = false;

                // Read stdout and stderr with streaming
                use std::io::{BufRead, BufReader};

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();

                // Use Arc for thread-safe cancellation flag
                let cancelled_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let cancelled_flag_clone = cancelled_flag.clone();
                let pid_clone = pid;

                // Spawn thread to listen for 'c' key
                let key_thread = std::thread::spawn(move || {
                    let _ = terminal::enable_raw_mode();
                    loop {
                        if cancelled_flag_clone.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                        if event::poll(std::time::Duration::from_millis(100)).unwrap_or(false) {
                            if let Ok(Event::Key(key)) = event::read() {
                                if key.code == KeyCode::Char('c') {
                                    let _ = terminal::disable_raw_mode();
                                    print!("\n{}", "Cancel? (y/N): ".yellow());
                                    io::stdout().flush().ok();

                                    let mut cancel_input = String::new();
                                    io::stdin().read_line(&mut cancel_input).ok();

                                    if cancel_input.trim().to_lowercase() == "y" {
                                        println!("{}", "Cancelling...".yellow());
                                        // Kill the process
                                        #[cfg(unix)]
                                        {
                                            let _ = std::process::Command::new("kill")
                                                .arg("-TERM")
                                                .arg(pid_clone.to_string())
                                                .spawn();
                                        }
                                        #[cfg(windows)]
                                        {
                                            // let _ = child.kill();
                                        }
                                        cancelled_flag_clone
                                            .store(true, std::sync::atomic::Ordering::SeqCst);
                                        break;
                                    } else {
                                        println!("{}", "Continuing...".green());
                                        let _ = terminal::enable_raw_mode();
                                    }
                                }
                            }
                        }
                    }
                    let _ = terminal::disable_raw_mode();
                });

                if let Some(stdout) = stdout {
                    let reader = BufReader::new(stdout);
                    for line in reader.lines() {
                        if cancelled_flag.load(std::sync::atomic::Ordering::SeqCst) {
                            cancelled = true;
                            break;
                        }
                        if let Ok(line) = line {
                            println!(":  {}\r", line.dimmed());
                            result.push_str(&line);
                            result.push('\n');
                        }
                    }
                }

                if !cancelled {
                    if let Some(stderr) = stderr {
                        let reader = BufReader::new(stderr);
                        for line in reader.lines() {
                            if cancelled_flag.load(std::sync::atomic::Ordering::SeqCst) {
                                cancelled = true;
                                break;
                            }
                            if let Ok(line) = line {
                                println!("  {} {}\r", "⚠".yellow(), line);
                                result.push_str("stderr: ");
                                result.push_str(&line);
                                result.push('\n');
                            }
                        }
                    }
                }

                // Signal key thread to stop and wait for it
                cancelled_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = key_thread.join();

                let status = child.wait().ok();

                if cancelled {
                    return Ok("Command execution cancelled by user".to_string());
                }

                if let Some(status) = status {
                    if status.success() {
                        if result.is_empty() {
                            Ok("Command executed successfully (no output)".to_string())
                        } else {
                            Ok(result.trim().to_string())
                        }
                    } else {
                        Ok(format!(
                            "Command exited with code {:?}\n{}",
                            status.code(),
                            result.trim()
                        ))
                    }
                } else {
                    Ok(result.trim().to_string())
                }
            }
            _ => {
                // Check if it's an MCP tool
                if name.starts_with("mcp_") {
                    self.execute_mcp_tool(name, arguments).await
                } else {
                    Err(anyhow!("Unknown tool: {}", name))
                }
            }
        }
    }

    async fn execute_mcp_tool(&self, full_name: &str, arguments: &str) -> Result<String> {
        // Parse MCP tool name: mcp_{server}_{tool}
        let parts: Vec<&str> = full_name.splitn(3, '_').collect();
        if parts.len() < 3 {
            bail!("Invalid MCP tool name format: {}", full_name);
        }

        let server_name = parts[1];
        let tool_name = parts[2];

        let mcp_clients = self.mcp_clients.read().await;
        let mcp_client = mcp_clients
            .iter()
            .find(|c| c.name == server_name)
            .ok_or_else(|| anyhow!("MCP server '{}' not found", server_name))?;

        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| anyhow!("Invalid tool arguments: {}", e))?;

        use rmcp::model::CallToolRequestParams;

        let arguments = args.as_object().cloned();

        let params = CallToolRequestParams {
            name: tool_name.to_string().into(),
            arguments,
            meta: None,
            task: None,
        };

        let result = mcp_client
            .peer
            .call_tool(params)
            .await
            .map_err(|e| anyhow!("MCP tool execution failed: {}", e))?;

        let content_str = result
            .content
            .into_iter()
            .map(|c| {
                use rmcp::model::RawContent;
                match c.raw {
                    RawContent::Text(text) => text.text,
                    RawContent::Image(img) => format!("[Image: {}]", img.mime_type),
                    RawContent::Resource(res) => format!("[Resource: {:?}]", res.resource),
                    RawContent::Audio(audio) => format!("[Audio: {}]", audio.mime_type),
                    RawContent::ResourceLink(link) => format!("[Resource Link: {:?}]", link),
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(if content_str.is_empty() {
            "No result".to_string()
        } else {
            content_str
        })
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

    fn read_input_with_ctrl_e(&self) -> Result<String> {
        // Enable raw mode to detect key presses
        terminal::enable_raw_mode()?;

        let mut input = String::new();
        let mut cursor_pos = 0usize;
        let mut editor_opened = false;

        // Helper to redraw the input line
        let redraw = |input: &str, cursor_pos: usize| {
            // Move to beginning of line, clear to end, print prompt and input
            print!("\r\x1B[K{} ", "You:".cyan().bold());
            print!("{}", input);
            // Move cursor to correct position
            if cursor_pos < input.len() {
                let move_left = input.len() - cursor_pos;
                print!("\x1B[{}D", move_left);
            }
            io::stdout().flush().ok();
        };

        loop {
            if event::poll(std::time::Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) => {
                        match (key.modifiers, key.code) {
                            // Ctrl+E: open editor
                            (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                                terminal::disable_raw_mode()?;
                                println!("\n{}", "Opening editor...".yellow());

                                match open_editor() {
                                    Ok(content) => {
                                        if !content.is_empty() {
                                            input = content;
                                            cursor_pos = input.len();
                                            editor_opened = true;
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("{} {}", "Error:".red(), e);
                                    }
                                }

                                // Re-enable raw mode to continue reading
                                terminal::enable_raw_mode()?;
                                if editor_opened && !input.is_empty() {
                                    redraw(&input, cursor_pos);
                                }
                            }
                            // Ctrl+C: exit
                            (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                                terminal::disable_raw_mode()?;
                                println!("\n{}", "Goodbye! 👋".green());
                                std::process::exit(0);
                            }
                            // Enter: submit input
                            (_, KeyCode::Enter) => {
                                terminal::disable_raw_mode()?;
                                println!();
                                return Ok(input);
                            }
                            // Backspace
                            (_, KeyCode::Backspace) => {
                                if cursor_pos > 0 {
                                    input.remove(cursor_pos - 1);
                                    cursor_pos -= 1;
                                    redraw(&input, cursor_pos);
                                }
                            }
                            // Delete
                            (_, KeyCode::Delete) => {
                                if cursor_pos < input.len() {
                                    input.remove(cursor_pos);
                                    redraw(&input, cursor_pos);
                                }
                            }
                            // Left arrow
                            (_, KeyCode::Left) => {
                                if cursor_pos > 0 {
                                    cursor_pos -= 1;
                                    redraw(&input, cursor_pos);
                                }
                            }
                            // Right arrow
                            (_, KeyCode::Right) => {
                                if cursor_pos < input.len() {
                                    cursor_pos += 1;
                                    redraw(&input, cursor_pos);
                                }
                            }
                            // Home
                            (_, KeyCode::Home) => {
                                cursor_pos = 0;
                                redraw(&input, cursor_pos);
                            }
                            // End
                            (_, KeyCode::End) => {
                                cursor_pos = input.len();
                                redraw(&input, cursor_pos);
                            }
                            // Regular character
                            (_, KeyCode::Char(c)) => {
                                if cursor_pos == input.len() {
                                    input.push(c);
                                } else {
                                    input.insert(cursor_pos, c);
                                }
                                cursor_pos += 1;
                                redraw(&input, cursor_pos);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            // If editor was opened and we have content, return it
            if editor_opened && !input.is_empty() {
                terminal::disable_raw_mode()?;
                return Ok(input);
            }
        }
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
            let mut system_content = self.args.system.clone();

            // Add agent context if available
            if let Some(ref agent_context) = self.agent_context {
                if agent_context.has_content() {
                    system_content.push_str("\n\n---\n# Agent Configuration\n");
                    system_content.push_str(&agent_context.to_system_prompt());
                }
            }

            system_content.push_str("\n\nCurrent time: ");
            system_content.push_str(&chrono::Local::now().to_rfc3339());

            let mut messages = vec![Message {
                role: "system".to_string(),
                content: Some(system_content),
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
            let mut first_content = true; // Track if we've received any content yet

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
                                    // Clear "thinking..." on first content
                                    if first_content {
                                        print!("\r{} ", "Assistant:".green().bold());
                                        io::stdout().flush()?;
                                        first_content = false;
                                    }
                                    print!("{}", content);
                                    io::stdout().flush()?;
                                    full_response.push_str(content);
                                }

                                // Handle tool calls
                                if let Some(delta_tool_calls) = &choice.delta.tool_calls {
                                    // Clear "thinking..." if tool calls arrive first
                                    if first_content {
                                        print!("\r{} ", "Assistant:".green().bold());
                                        io::stdout().flush()?;
                                        first_content = false;
                                    }

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
                // println!("{}", "Executing tools...".yellow().bold());

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

                // println!("{}", "Processing results...".yellow().bold());
                print!("{} ", "Assistant:".green().bold());
                print!("{}", "thinking...".dimmed().italic());
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

        if let Some(ref mcp_path) = self.args.mcp {
            println!(
                "  {} {}",
                "MCP config:".dimmed(),
                mcp_path.display().to_string().dimmed()
            );
            println!("  {} {}", "MCP tools:".dimmed(), "enabled".green());

            // Access mcp_clients to show connected servers
            let clients = self.mcp_clients.read().await;
            if !clients.is_empty() {
                println!(
                    "  {} {} MCP server(s) connected",
                    "•".dimmed(),
                    clients.len()
                );
            }
        }

        println!();
        println!("{}", "Type your message and press Enter to chat.".dimmed());
        println!(
            "{}",
            "Type 'exit', 'quit', or press Ctrl+C to exit.".dimmed()
        );
        println!("{}", "Type 'clear' to clear conversation history.".dimmed());
        println!(
            "{}",
            "Press Ctrl+E to open editor for multi-line input.".dimmed()
        );
        println!();

        loop {
            // Check for Ctrl+E before normal input
            print!("{} ", "You:".cyan().bold());
            io::stdout().flush()?;

            let input = self.read_input_with_ctrl_e()?;

            let trimmed = input.trim();

            if trimmed.is_empty() {
                continue;
            }

            match trimmed.to_lowercase().as_str() {
                "exit" | "quit" | "q" => {
                    if let Some(ref workspace) = self.workspace {
                        save_history(workspace, &self.history);
                    }
                    println!("\n{}", "Goodbye! 👋".green());
                    break;
                }
                "clear" => {
                    self.history.clear();
                    if let Some(ref workspace) = self.workspace {
                        save_history(workspace, &self.history);
                    }
                    println!("{}", "Conversation history cleared.".yellow());
                    continue;
                }
                _ => {}
            }

            print!("{} ", "Assistant:".green().bold());
            print!("{}", "thinking...".dimmed().italic());
            io::stdout().flush()?;

            if let Err(e) = self.send_message(trimmed).await {
                eprintln!("\n{} {}", "Error:".red(), e);
            }
            println!();

            // Save history after each message exchange
            if let Some(ref workspace) = self.workspace {
                save_history(workspace, &self.history);
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();

    if args.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Load config file if specified
    if let Some(config_path) = &args.config {
        if let Ok(config_content) = std::fs::read_to_string(config_path) {
            if let Ok(config) = serde_yaml::from_str::<ConfigFile>(&config_content) {
                // Validate preset exists if specified
                if let Some(preset_name) = &args.preset {
                    if !config.presets.contains_key(preset_name) {
                        eprintln!(
                            "{} Preset '{}' not found. Available presets: {}",
                            "Error:".red(),
                            preset_name,
                            config
                                .presets
                                .keys()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        std::process::exit(1);
                    }
                }

                // Helper function to apply config value
                // Priority: CLI args > Config preset > Config global > Environment variables
                let apply_config = |key: &str, env_name: &str| -> Option<String> {
                    // Check preset (if specified and exists)
                    if let Some(preset_name) = &args.preset {
                        if let Some(preset) = config.presets.get(preset_name) {
                            if let Some(value) = preset.get(key) {
                                if !value.is_empty() {
                                    return Some(value.clone());
                                }
                            }
                        }
                    }
                    // Check global (only if no preset or preset value was empty)
                    if args.preset.is_none() {
                        if let Some(value) = config.global.get(key) {
                            if !value.is_empty() {
                                return Some(value.clone());
                            }
                        }
                    }
                    // Check environment variable
                    if let Ok(value) = env::var(env_name) {
                        if !value.is_empty() {
                            return Some(value);
                        }
                    }
                    None
                };

                // Apply config values (only if CLI arg was not explicitly provided)
                // Check if arg was provided by comparing with default value
                let default_args = Args::parse_from(std::iter::empty::<String>());

                if args.endpoint == default_args.endpoint {
                    if let Some(endpoint) = apply_config("UNDEAD_ENDPOINT", "UNDEAD_ENDPOINT") {
                        args.endpoint = endpoint;
                    }
                }
                if args.model == default_args.model {
                    if let Some(model) = apply_config("UNDEAD_MODEL", "UNDEAD_MODEL") {
                        args.model = model;
                    }
                }
                if args.api_key == default_args.api_key {
                    if let Some(api_key) = apply_config("UNDEAD_API_KEY", "UNDEAD_API_KEY") {
                        args.api_key = api_key;
                    }
                }
                if args.system == default_args.system {
                    if let Some(system) = apply_config("UNDEAD_SYSTEM", "UNDEAD_SYSTEM") {
                        args.system = system;
                    }
                }
                if (args.temperature - default_args.temperature).abs() < f32::EPSILON {
                    if let Some(temperature) =
                        apply_config("UNDEAD_TEMPERATURE", "UNDEAD_TEMPERATURE")
                    {
                        if let Ok(temp) = temperature.parse() {
                            args.temperature = temp;
                        }
                    }
                }
                if args.max_tokens == default_args.max_tokens {
                    if let Some(max_tokens) = apply_config("UNDEAD_MAX_TOKENS", "UNDEAD_MAX_TOKENS")
                    {
                        if let Ok(tokens) = max_tokens.parse() {
                            args.max_tokens = tokens;
                        }
                    }
                }
                if args.workspace == default_args.workspace {
                    if let Some(workspace) = apply_config("UNDEAD_WORKSPACE", "UNDEAD_WORKSPACE") {
                        args.workspace = Some(PathBuf::from(workspace));
                    }
                }
                if args.mcp == default_args.mcp {
                    if let Some(mcp) = apply_config("UNDEAD_MCP", "UNDEAD_MCP") {
                        args.mcp = Some(PathBuf::from(mcp));
                    }
                }
            } else {
                eprintln!("{} Failed to parse config file", "Warning:".yellow());
            }
        } else {
            eprintln!(
                "{} Failed to read config file: {}",
                "Warning:".yellow(),
                config_path.display()
            );
        }
    }

    let mut app = ChatApp::new(args);

    // Initialize MCP servers if configured
    if let Err(e) = app.initialize_mcp().await {
        eprintln!("{} Failed to initialize MCP: {}", "Warning:".yellow(), e);
    }

    app.run().await
}
