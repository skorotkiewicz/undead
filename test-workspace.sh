#!/bin/bash

# Test script for undead workspace feature
# This script demonstrates the file operation capabilities when using --workspace

set -e

echo "🧟 Undead Workspace Feature Test"
echo "================================="
echo ""

# Create a test workspace directory
WORKSPACE_DIR="./test-workspace-demo"
rm -rf "$WORKSPACE_DIR"
mkdir -p "$WORKSPACE_DIR"

echo "✓ Created test workspace: $WORKSPACE_DIR"
echo ""

# Create some sample files
echo "Hello, this is a test file!" > "$WORKSPACE_DIR/hello.txt"
echo "Another sample file" > "$WORKSPACE_DIR/sample.txt"
mkdir -p "$WORKSPACE_DIR/subdir"

echo "✓ Created sample files in workspace"
echo ""

# Show workspace contents
echo "Workspace contents:"
ls -la "$WORKSPACE_DIR"
echo ""

echo "=========================================="
echo "Starting undead with workspace enabled..."
echo "=========================================="
echo ""
echo "The LLM now has access to these tools:"
echo "  • read_file - Read file contents"
echo "  • write_file - Write/create files"
echo "  • create_directory - Create directories"
echo "  • delete_file - Delete files"
echo "  • delete_directory - Delete directories"
echo "  • list_directory - List directory contents"
echo ""
echo "Example prompts you can try:"
echo "  1. 'List the files in the current directory'"
echo "  2. 'Read the contents of hello.txt'"
echo "  3. 'Create a new file called notes.txt with some content'"
echo "  4. 'Create a directory called projects'"
echo "  5. 'Delete the sample.txt file'"
echo ""
echo "Note: All operations are restricted to the workspace directory."
echo "      The LLM cannot access files outside of $WORKSPACE_DIR"
echo ""
echo "Press Ctrl+C to exit when done."
echo ""

# Start undead with workspace
./target/release/undead \
    --workspace "$WORKSPACE_DIR" \
    --endpoint "http://192.168.0.124:8888/v1" \
    --model "local-model" \
    --system "You are a helpful assistant with file system access. You can read, write, create, and delete files in the workspace directory. Always confirm actions before performing destructive operations like deletions."

# Cleanup (optional - comment out if you want to keep the workspace)
# echo ""
# echo "Cleaning up test workspace..."
# rm -rf "$WORKSPACE_DIR"
# echo "✓ Cleanup complete"
