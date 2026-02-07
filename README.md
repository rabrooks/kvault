# kvault

Searchable knowledge corpus with BM25 ranking. Rust library, CLI, and MCP server.

## Status

CLI, search, and MCP server are functional. Optional Tantivy (BM25) ranked search backend available via `ranked` feature.

## What is kvault?

kvault is a fast, local-first knowledge base that lets you:

- **Store** knowledge as Markdown, JSON, or plain text
- **Search** with ripgrep (fast) or Tantivy BM25 ranking (optional)
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

# With MCP server support
cargo build --release --features mcp

# With ranked search (BM25 + fuzzy matching)
cargo build --release --features ranked

# Full build with all features
cargo build --release --features "ranked,mcp"
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
kvault search <query>          # Search the corpus (case-insensitive)
kvault search <query> -l 5     # Limit results
kvault search <query> -c aws   # Filter by category
kvault search <query> -s       # Case-sensitive search
kvault search <query> -b ranked # Use BM25 ranked search (requires --features ranked)
kvault search <query> --fuzzy  # Fuzzy search with edit distance 1 (ranked backend)
kvault search <query> --fuzzy 2 # Fuzzy search with edit distance 2
kvault list                    # List all documents
kvault list --category aws     # Filter by category
kvault get <path>              # Print document contents
kvault index                   # Build search index (requires --features ranked)
kvault serve                   # Start MCP server (requires --features mcp)
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

### Environment Variables

| Variable | Description |
|----------|-------------|
| `KVAULT_CONFIG` | Override config file location (useful for testing) |

## Storage Backends

| Backend | Use Case | Status |
|---------|----------|--------|
| Local filesystem | CLI users, scripts, personal knowledge | Available |
| S3 | Team sharing, distributed corpus | Planned |

## Search Backends

| Backend | Use Case | Status |
|---------|----------|--------|
| ripgrep | Fast text search, no indexing needed | Available (default) |
| Tantivy | BM25 ranked results, fuzzy search, requires indexing | Available (`ranked` feature) |

### Ranked Search (Tantivy)

Build with ranked search support:

```bash
cargo build --release --features ranked
```

Build the search index (required before using ranked search):

```bash
kvault index
```

Use ranked search:

```bash
# Explicit ranked backend
kvault search "lambda patterns" --backend ranked

# Auto-select (uses ranked if index exists, else ripgrep)
kvault search "lambda patterns" --backend auto

# Fuzzy search - finds "lambda" even if you type "lamda"
kvault search "lamda" --fuzzy        # 1 edit distance (default)
kvault search "lamda" --fuzzy 2      # 2 edit distance (more permissive)
```

**Edit distance guide:**

| Distance | Catches | Example |
|----------|---------|---------|
| 1 | Single typo, missing/extra char | `lamda` matches `lambda` |
| 2 | Two typos, transpositions | `lambada` matches `lambda` |

## MCP Server

kvault includes an MCP server for AI editor integration. Build with MCP support:

```bash
cargo build --release --features mcp
```

Add to your editor's MCP configuration:

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

### Supported Editors

- Claude Code
- Cursor
- Windsurf
- Any editor supporting the Model Context Protocol

### MCP Tools

| Tool | Description |
|------|-------------|
| `search_knowledge` | Search the corpus for matching documents |
| `list_knowledge` | List all documents, optionally filtered by category |
| `get_document` | Get full contents of a document by path |
| `add_knowledge` | Add a new document to the corpus |

## Feature Flags

| Flag | Description |
|------|-------------|
| `ranked` | Enable Tantivy BM25 ranked search with fuzzy matching |
| `mcp` | Enable MCP server (`kvault serve`) |

## License

MIT
