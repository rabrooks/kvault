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

1. Create a knowledge corpus:

```bash
mkdir -p ~/.claude/knowledge/aws
```

2. Add a manifest.json:

```json
{
  "version": "1",
  "documents": [
    {
      "path": "aws/lambda-patterns.md",
      "title": "AWS Lambda Patterns",
      "category": "aws",
      "tags": ["lambda", "serverless"]
    }
  ]
}
```

3. Add your knowledge documents, then search:

```bash
kvault list
kvault search "lambda"
kvault get aws/lambda-patterns.md
```

## CLI Commands

```
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
  "~/.claude/knowledge",      # global knowledge
  "./.claude/knowledge",      # project-specific
]
```

Default paths are used if no config file exists.

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
