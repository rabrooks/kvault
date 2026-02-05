# kvault

Searchable knowledge corpus with BM25 ranking. Rust library, CLI, and MCP server.

## Status

Early development — not yet functional.

## What is kvault?

kvault is a fast, local-first knowledge base that lets you:

- **Store** knowledge as Markdown, JSON, or plain text
- **Search** with ranked results using BM25 (powered by Tantivy)
- **Access** via CLI, Rust library, or MCP server

No external AI services. No embeddings APIs. You control where your knowledge lives.

## Storage Backends

| Backend | Use Case |
|---------|----------|
| Local filesystem | CLI users, scripts, personal knowledge |
| S3 | Team sharing, distributed corpus |

## Editor Integration

kvault exposes an MCP server that works with any editor supporting the Model Context Protocol:

- Claude Code
- Cursor
- Windsurf
- opencode
- Any MCP-compatible client

## Planned Features

- Full-text search with BM25 ranking
- Multiple storage backends (local, S3)
- Manifest-based metadata (keeps your documents clean)
- MCP server for AI editor integration
- CLI for scripting and manual use

## License

MIT
