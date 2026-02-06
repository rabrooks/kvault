# kvault

Searchable knowledge corpus with BM25 ranking. Rust library, CLI, and MCP server.

## Status

Early development — CLI and search functional, MCP server in progress.

## What is kvault?

kvault is a fast, local-first knowledge base that lets you:

- **Store** knowledge as Markdown, JSON, or plain text
- **Search** with ripgrep (fast) or BM25 ranking (coming soon)
- **Access** via CLI, Rust library, or MCP server

No external AI services. No embeddings APIs. You control where your knowledge lives.

## Installation

Requires [ripgrep](https://github.com/BurntSushi/ripgrep) for search:

```bash
brew install ripgrep    # macOS
cargo install ripgrep   # any platform
apt install ripgrep     # Debian/Ubuntu
```

Install kvault:

```bash
cargo install kvault
```

Or build from source:

```bash
git clone https://github.com/aaronbrooks/kvault
cd kvault
cargo build --release
```

## Quick Start

Add a document:

```bash
echo "# Lambda Patterns

Use environment variables for configuration.
Keep functions small and focused.
" | kvault add --title "AWS Lambda Patterns" --category aws --tags "lambda,serverless"
```

Or from a file:

```bash
kvault add --title "AWS Lambda Patterns" --category aws --file ./notes.md
```

Then search and retrieve:

```bash
kvault list                           # List all documents
kvault search "environment"           # Search content
kvault get aws/aws-lambda-patterns.md # View full document
```

## CLI Commands

```
kvault add --title "..." --category "..." [--tags "..."] [--file path]
                               # Add document (reads stdin if no --file)
kvault search <query>          # Search the corpus
kvault search <query> -l 5     # Limit results
kvault list                    # List all documents
kvault list --category aws     # Filter by category
kvault get <path>              # Print document contents
```

## Configuration

Config file: `~/.config/kvault/config.toml`

```toml
[corpus]
paths = [
  "~/.kvault",              # default location
  "./.kvault",              # add project-specific paths as needed
  "~/work/shared-kb",       # or team/custom locations
]
```

Default: `~/.kvault` is used if no config file exists.

## Storage Backends

| Backend | Use Case | Status |
|---------|----------|--------|
| Local filesystem | CLI users, scripts, personal knowledge | ✓ |
| S3 | Team sharing, distributed corpus | Planned |

## Search Backends

| Backend | Use Case | Status |
|---------|----------|--------|
| ripgrep | Fast text search, no indexing needed | ✓ |
| Tantivy | BM25 ranked results, requires indexing | Planned |

## Editor Integration

kvault exposes an MCP server that works with any editor supporting the Model Context Protocol:

- Claude Code
- Cursor
- Windsurf
- opencode

```json
{
  "mcpServers": {
    "kvault": {
      "command": "kvault",
      "args": ["serve"]
    }
  }
}
```

MCP server support is in development.

## License

MIT
