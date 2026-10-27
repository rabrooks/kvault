# kvault

Local-first knowledge index for coding agents. Markdown files on disk are the only source of
truth; kvault derives a disposable SQLite index (FTS5 over heading-level chunks, links, tags)
and gives agents token-budgeted retrieval through a CLI, Claude Code hooks, and an optional
MCP adapter.

## Status

**v0 rebuild in progress** on the `rebuild/v0` branch. The 0.1 manifest/ripgrep/Tantivy code
was removed; it never saw real use. The skeleton builds, the schema is live, and commands land
milestone by milestone:

| Milestone | Delivers | State |
|---|---|---|
| M0 | toolchain pin, module tree, schema v1, `status`, CI | done |
| M1 | `init`, `index`, `list` over real roots (incremental, rename-safe) | done |
| M2 | `search`, `get`, `--explain`, `bench` | next |
| M3 | `recall` budget packer, `brief` | |
| M4 | Claude Code hooks, `SKILL.md`, `add`, session capture | |
| M5 | links stream, `links` | |
| M6 | MCP stdio adapter (`--features mcp`) | |

Commands that are not implemented yet exit with code 3 and name their milestone.

## Why

Knowledge for agent work ends up in several places at once: Claude Code auto-memory dirs,
per-project `notes/` folders, reference notes, `CLAUDE.md`. Nothing searches across them, and
the agent only sees what fits in a session-start file. kvault indexes those directories
*in place*, never moves or rewrites a file, and answers "what did we decide about X" in one
command sized to a token budget.

Design principles: markdown is the only truth; index what exists (frontmatter optional);
budgets are tokens, not counts; hooks are fast and silent; no LLM and no daemon in the loop;
ranking is explainable; the core stays small.

## Build

Requires Rust 1.95 (pinned in `rust-toolchain.toml`; `rustup` picks it up).

```bash
cargo build --release
cargo test
```

## Configuration

`~/.config/kvault/config.toml` (override with `KVAULT_CONFIG`; index path with `KVAULT_DB`):

```toml
[[roots]]
path = "~/.claude/projects/*/memory"   # globs allowed
kind = "memory"
weight = 1.2
project_from = "claude_slug"

[[roots]]
path = "~/Projects/*/notes"
kind = "notes"
project_from = "parent"

[[roots]]
path = "~/Projects/notes"
kind = "reference"
weight = 0.8

[brief]
budget = 2000
recent_days = 14
```

`kvault init` writes this for you (only roots that exist on disk) and runs the first index.

## Indexing

`kvault index` is incremental: files whose mtime and size match are skipped without being
read; changed files are hashed with BLAKE3 and re-parsed only if the hash differs. A file that
moves keeps its document id. Deleted files are soft-deleted so a restored file reclaims its id.
A root that is missing on disk is skipped, never treated as a mass delete. Git ignore files are
not honoured inside roots (they are often inside ignored directories); a `.kvaultignore` is.

Documents are chunked by heading. Sections over 1,200 tokens split at the best blank line or
rule, never inside a code fence; sections under 40 tokens merge into the next one. Frontmatter is
optional: `title`, `description`, `kind`/`type`, `tags`, `created`, `updated`, `pinned` are
lifted, everything else is kept as filterable metadata. `[[wikilinks]]`, relative `.md` links,
`- verb [[Target]]` relations, and `- [category] text #tag` observation lines are extracted.

On a corpus of 226 files / 1.3 MB: cold index ~200 ms, warm ~4 ms.

## CLI (v0 surface)

```
kvault init                                  write config, index
kvault index [--full] [--root P]             incremental reindex
kvault search <q> [--project P] [--tag T] [--path G] [--since 30d] [--explain]
kvault recall <q> --budget N [--project P]   search packed to a token budget (agents call this)
kvault get <id|path>[:from[:count]]          print a chunk or line window
kvault brief [--cwd P] [--budget N]          what the SessionStart hook injects
kvault add --title T [--kind K] [--tags a,b] write a note and index it
kvault list / links / status / bench
kvault hook session-start|stop|pre-compact   Claude Code hook entry points
kvault serve                                 MCP stdio (feature `mcp`)
```

## License

MIT
