# 🧟 Undead - LLM Chat Client

A simple and elegant CLI chat application for OpenAI-compatible APIs. Perfect for interacting with local LLM servers like llama.cpp, ollama, text-generation-webui, and more.

![intro.png](docs/intro.png)

## Features

- 🎯 **Simple & Elegant** - Clean, intuitive CLI interface
- 💬 **Interactive Chat** - Real-time conversation with conversation history
- 🌊 **Streaming Responses** - See responses as they're generated
- 🎨 **Colored Output** - Beautiful, readable terminal output
- 🔧 **Flexible Configuration** - Customize API endpoint, model, temperature, and more
- 📜 **Conversation History** - Maintains context throughout the chat session
- 🔐 **API Key Support** - Works with both local and remote APIs

## Installation

```bash
cargo build --release
```

The binary will be available at `target/release/undead`.

## Quick Start

### Basic Usage (Local Server)

```bash
./undead
```

This connects to `http://localhost:8080/v1` by default.

### With Custom API Endpoint

```bash
./undead --endpoint http://localhost:11434/v1 --model llama2
```

### With API Key (Remote Services)

```bash
./undead --endpoint https://api.openai.com/v1 --api-key sk-your-key-here --model gpt-3.5-turbo
```

## Command Line Options

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--endpoint` | `-e` | `http://localhost:8080/v1` | API base endpoint |
| `--model` | `-m` | `local-model` | Model to use for chat |
| `--api-key` | `-k` | (empty) | API key (optional for local servers) |
| `--system` | `-s` | `You are a helpful assistant.` | System prompt |
| `--temperature` | (none) | `0.7` | Response randomness (0.0-2.0) |
| `--max-tokens` | `-t` | `2048` | Maximum tokens in response |
| `--workspace` | `-w` | (none) | Workspace directory for file operations |
| `--mcp` | (none) | (none) | MCP configuration file path |

## Workspace Feature

When you specify a `--workspace` directory, the LLM gains access to file operation tools. This allows the assistant to read, write, create, and delete files and directories within the specified workspace.

### Available Tools

When workspace is enabled, the LLM can use these tools:

- **read_file** - Read the contents of a file
- **write_file** - Write content to a file (creates or overwrites)
- **create_directory** - Create a new directory (with parent directories if needed)
- **delete_file** - Delete a file
- **delete_directory** - Delete an empty directory
- **list_directory** - List contents of a directory

### Security

All file operations are restricted to the workspace directory. The application validates paths to prevent directory traversal attacks, ensuring the LLM cannot access files outside the specified workspace.

### Example Usage

```bash
# Enable workspace for a project directory
./undead --workspace ./src

# The LLM can now read and modify files in ./src
# Example conversation:
# You: "List the files in the current directory"
# Assistant: [Uses list_directory tool]
# You: "Create a new file called test.txt with 'Hello World'"
# Assistant: [Uses write_file tool]
```

## MCP Feature

Model Context Protocol (MCP) support allows the LLM to connect to external tool servers. This extends the capabilities beyond file operations to include databases, APIs, web scraping, and more.

### MCP Configuration File

Create a JSON configuration file (e.g., `mcp.json`) with your MCP servers:

```json
{
  "mcp_servers": {
    "city": {
      "type": "remote",
      "url": "https://mcp.example.com/mcp?key=sk_5af",
      "enabled": true
    },
    "filesystem": {
      "type": "local",
      "command": "mcp-server-filesystem",
      "args": ["--root", "/home/user/documents"],
      "env": {},
      "enabled": true
    },
    "github": {
      "type": "local",
      "command": "mcp-server-github",
      "args": [],
      "env": {
        "GITHUB_TOKEN": "your-github-token-here"
      },
      "enabled": true
    },
    "postgres": {
      "type": "local",
      "command": "mcp-server-postgres",
      "args": ["postgres://user:password@localhost:5432/mydb"],
      "env": {},
      "enabled": true
    }
  }
}
```

### Using MCP

```bash
# Enable MCP tools
./undead --mcp ./mcp.json

# Combine with workspace
./undead --workspace ./my-project --mcp ./mcp.json
```

### MCP Server Types

**Local MCP Servers** run as processes on your machine:

- `type`: "local" (optional, default)
- `command`: The executable command to run
- `args`: Command-line arguments (optional)
- `env`: Environment variables (optional)
- `enabled`: Whether the server is active (default: true)

**Remote MCP Servers** are hosted services accessible via HTTP:

- `type`: "remote" (required for remote servers)
- `url`: The HTTP endpoint for the MCP server
- `enabled`: Whether the server is active (default: true)

See the [MCP documentation](https://modelcontextprotocol.io) for more servers and details.

## Interactive Commands

During a chat session, you can use these commands:

- `exit`, `quit`, or `q` - Exit the application
- `clear` - Clear conversation history

## Examples

### Using with llama.cpp

```bash
# Start llama.cpp server
./server -m model.gguf --port 8080

# Connect with undead
./undead --endpoint http://localhost:8080/v1
```

### Using with Ollama

```bash
# Start Ollama
ollama serve

# Connect with undead
./undead --endpoint http://localhost:11434/v1 --model llama2
```

### Using with text-generation-webui

```bash
# Start text-generation-webui with OpenAI extension
python server.py --extensions openai

# Connect with undead
./undead --endpoint http://localhost:5000/v1
```

### Using with OpenAI API

```bash
./undead \
  --endpoint https://api.openai.com/v1 \
  --api-key sk-your-api-key \
  --model gpt-3.5-turbo \
  --temperature 0.8
```

### Custom System Prompt

```bash
./undead --system "You are a Rust programming expert. Provide concise, accurate answers."
```

### With Workspace for File Operations

```bash
# Enable file operations in a specific directory
./undead --workspace ./my-project

# Combine with other options
./undead --workspace ./src --endpoint http://localhost:11434/v1 --model llama2
```

## Configuration Examples

### Creative Writing Assistant

```bash
./undead \
  --temperature 1.2 \
  --system "You are a creative writing assistant. Be imaginative and inspiring."
```

### Code Review Assistant

```bash
./undead \
  --temperature 0.3 \
  --system "You are a code reviewer. Focus on best practices, security, and performance."
```

## Keyboard Shortcuts

- `Ctrl+C` - Exit the application
- `Enter` - Send message

## Requirements

- Rust 1.70 or later
- An OpenAI-compatible API server (local or remote)

## Supported API Servers

Any server that implements the OpenAI Chat Completions API:

- llama.cpp
- Ollama
- text-generation-webui
- vLLM
- LocalAI
- OpenAI API
- Azure OpenAI
- Anthropic (via proxy)
- And many more!

## License

MIT

## Contributing

Contributions are welcome! Feel free to submit issues and pull requests.
