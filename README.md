# kvault

Local-first knowledge index for coding agents. Markdown files on disk are the only source of
truth; kvault derives a disposable SQLite index (FTS5 over heading-level chunks, links, tags)
and gives agents token-budgeted retrieval through a CLI, Claude Code hooks, and an optional
MCP adapter.

## Status

**v0 rebuild in progress.** The 0.1 manifest/ripgrep/Tantivy code was removed; it never saw
real use. Commands land milestone by milestone:

| Milestone | Delivers | State |
|---|---|---|
| M0 | toolchain pin, module tree, schema v1, `status`, CI | done |
| M1 | `init`, `index`, `list` over real roots (incremental, rename-safe) | done |
| M2 | `search`, `get`, `--explain`, `bench` | done |
| M3 | `recall` budget packer, `brief` | done |
| M4 | Claude Code hooks, `SKILL.md`, `add`, session capture | next |
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

## Searching

The agent writes the query; kvault does no expansion of its own.

```
kvault search "lambda cold start"            # bare words: any may match, each prefix-matched
kvault search '"cold start" -timeout'        # quoted = phrase, -word excludes
kvault search 'tag:devops symplr'            # inline tag filter; -tag:x excludes
kvault search 'tag:devops'                   # filters alone list matching docs, newest first
kvault search 'lex: title:lambda NEAR(cold start)'   # raw FTS5 syntax passed through
kvault search ... --project vllm --kind memory --path 'research/*' --since 30d
kvault search ... --format cli|json|md|paths|ids --explain --per-chunk -n 20
```

Four streams run independently and are fused by reciprocal rank fusion (k=60): all-terms
FTS (weight 2), any-term FTS, exact title match, and tag match. Exclusions (`-word`, `-tag:x`,
`NOT` inside `lex:`) apply to every stream. A small authority multiplier (0.85–1.15) from the
root weight and `pinned: true` breaks ties. Results are one chunk per document with the other
matching chunks listed as siblings. `--explain` prints each hit's per-stream rank and raw
score, its fused score, and per-stream timings.

Hits carry an eight-hex-digit id, a prefix of a BLAKE3 over the file path, heading path and
chunk text, so it is stable until one of those changes and identical boilerplate sections in
different files still get different ids. `kvault get <id>` prints the chunk with line numbers;
`kvault get <path>[:from[:count]]` prints a line window; `--doc` prints the whole file. Paths
are accepted as printed by `search`, absolute, or as any unique suffix. If a file changed
since it was indexed the output is marked `[stale]`.

`kvault bench <queries.jsonl> [--baseline] [-v] [--min-hit5 F]` scores hit@1/3/5, section
hit@5, MRR and latency over a query file (`{"query", "expected": ["path#heading"]}` per line),
optionally against a case-insensitive grep over the same files. `--min-hit5` makes it a CI
gate. On a 44-query bench over the real corpus (release build): kvault hit@1 0.95, hit@5 1.00,
MRR 0.98, p50 1 ms; grep hit@1 0.59, hit@5 0.98, MRR 0.75, p50 10 ms.

## Recall and brief

`kvault recall "<query>" --budget 1500 [--project P] [--format md|json]` is `search` packed
into a token budget. It is what agents call. Each hit is shown at one of three tiers: a
one-line reference, the reference plus a two-line preview, or the whole chunk. The packer
starts with the top three hits in full and the next seven previewed, then sheds from the
bottom of the ranking (drop reference lines, downgrade previews, downgrade full chunks,
downgrade the remaining previews, and only then drop hits) until the whole output, framing
included, fits. A chunk is shown whole or not at all. The budget applies to the bytes
printed in whichever format was asked for, as measured by the index's token counter; the
output ends with the number of hits omitted (counted up to 200) and the `kvault get` hint.

Shown text is verbatim except for trimming: frontmatter is dropped, trailing whitespace and
runs of blank lines are collapsed. Nothing inside a line is rewritten, so `path:line`
citations and code fences survive. A chunk whose file changed since it was indexed is
marked `[stale]` and shown as a preview at most, never quoted in full.

`kvault brief [--cwd P] [--budget N]` prints the context block the SessionStart hook injects
for a working directory: pinned notes and rules, notes updated in the last 14 days, and
notes elsewhere that link to or share tags with the project, one line each with a chunk id.
The project is the nearest ancestor directory whose name matches a configured root's
project. The block fits the budget (2000 tokens by default) and stays under 8,000 characters
so Claude Code does not swap it for a file preview; the header line is byte-stable across
runs except the counts and a coarse index age at its end.

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
