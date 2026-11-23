# kvault

Shared memory for coding agents. kvault indexes the markdown notes you already keep and gives
Claude Code a session-start brief and token-budgeted search across every project. One Rust
binary, no daemon, no LLM.

Markdown files on disk are the only source of truth. kvault derives a disposable SQLite index
(FTS5 over heading-level chunks, links, tags) and serves it through a CLI and Claude Code
hooks; an MCP adapter is planned.

## Status

**0.2.0, beta, in trial use.** Indexing, search, budgeted recall, the session brief, the
Claude Code hooks and skill, session capture, `add`, the query log and `report` all work.
Two commands are declared but not built, and exit with code 3 saying so: `links` (link
neighbours of a note) and `serve` (an MCP stdio adapter, behind `--features mcp`). Links
are already extracted and used by the brief; there is no link-based ranking yet.

The on-disk index is derived and its schema may change between versions; it migrates on
open, and `rm ~/.local/share/kvault/index.db && kvault index` rebuilds it from the notes at
any time.

## Why

Knowledge for agent work ends up in several places at once: Claude Code auto-memory dirs,
per-project `notes/` folders, reference notes, `CLAUDE.md`. Nothing searches across them, and
the agent only sees what fits in a session-start file. kvault indexes those directories
*in place*, never moves or rewrites a file, and answers "what did we decide about X" in one
command sized to a token budget.

Design principles: markdown is the only truth; index what exists (frontmatter optional);
budgets are tokens, not counts; hooks are fast and silent; no LLM and no daemon in the loop;
ranking is explainable; the core stays small.

## Quick start

Requires Rust 1.95 or newer (`rustup update stable`).

```bash
cargo install kvault          # puts `kvault` on PATH (~/.cargo/bin)
kvault init                   # writes the config, then indexes
kvault status                 # roots found, document counts
kvault recall "something you know is in your notes"
```

`kvault init` always configures Claude Code's memory dirs (`~/.claude/projects/*/memory`)
and adds `~/Projects/*/notes` and `~/Projects/notes` when they exist. Tell it where your
notes are if that is somewhere else, at init or any time after:

```bash
kvault init --notes '~/code/*/notes' --notes ~/Documents/notes   # quote globs
kvault root add ~/work/handbook                                  # later additions
kvault root add '~/vaults/*' --kind reference --weight 0.8
```

`root add` appends to the config without touching what is there and reindexes. Each
project's name is worked out from the path: a `notes` or `docs` directory, or any fixed
name under a glob, belongs to the directory above it (`~/code/app/notes` is project `app`);
anything else is its own project (`~/work/handbook` is `handbook`). `--project-from
parent|dir|none` overrides that.

Then give Claude Code the hooks and the skill. Install the binary first: the plugin only
carries the hook definitions and the skill, and its hooks call `kvault` from `PATH`.

```
/plugin marketplace add rabrooks/kvault
/plugin install kvault@kvault
```

or, for a single session without installing, `claude --plugin-dir <path to a clone of this repo>`.

Nothing is uploaded anywhere. kvault never
moves or rewrites your notes: the only files it writes are session notes, notes you `add`,
and the pointer line `add` appends to `MEMORY.md` in a Claude Code memory dir.

To work on kvault itself, clone the repository (`rust-toolchain.toml` pins Rust 1.95, which
`rustup` picks up), then `cargo build --release` and `cargo test`. `cargo install --path .`
installs a build of the clone in place of the released one.

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

[[roots]]
path = "~/work/handbook"               # a directory that is itself the notes
kind = "notes"
project_from = "dir"                   # project = the directory's own name
demote = ["**/archive/**"]             # the default: these rank a little below their peers
demote_factor = 0.9

[projects.handbook]                    # one project known by several names
aliases = ["work-handbook"]            # e.g. the name its Claude memory slug derives

[capture]
enabled = true                         # session notes on Stop / PreCompact
session_dir = "notes/sessions"         # under the project, when it has a notes dir
fallback_dir = "~/.local/share/kvault/sessions"   # otherwise; indexed automatically

[brief]
budget = 2000
recent_days = 14

[query_log]
enabled = true
```

`kvault init` writes the memory root and whichever of the other two exist, plus any
`--notes` you pass, and runs the first index; `kvault root add <path>` appends one later.
`kind` is `memory`, `notes`, `reference` or `other`. `project_from` names
the project a root's files belong to: `parent` (the directory above, for `<project>/notes`),
`dir` (the root's own name), `claude_slug` (a Claude Code memory dir), or `none`. The
project is what `--project` filters on and how the brief knows which notes are "here";
when two roots name the same project differently, an alias joins them.

## Indexing

`kvault index` is incremental: files whose mtime and size match are skipped without being
read; changed files are hashed with BLAKE3 and re-parsed only if the hash differs. A file that
moves keeps its document id. Deleted files are soft-deleted so a restored file reclaims its id.
A root that is missing on disk is skipped, never treated as a mass delete: its documents are
hidden from every result until the directory is back, which costs nothing. `kvault status`
lists such roots, and `kvault index --prune` drops their rows when they are gone for good.
Git ignore files are not honoured inside roots (they are often inside ignored directories); a
`.kvaultignore` is.

Documents are chunked by heading. Sections over 1,200 tokens split at the best blank line or
rule, never inside a code fence; sections under 40 tokens merge into the next one. Frontmatter is
optional: `title`, `description`, `kind`/`type`, `tags`, `created`, `updated`, `pinned` are
lifted, everything else is kept as filterable metadata. `[[wikilinks]]`, relative `.md` links,
`- verb [[Target]]` relations, and `- [category] text #tag` observation lines are extracted.

On a corpus of about 630 files: re-hashing everything (`--full`) takes roughly 200 ms, an
unchanged run about 10 ms.

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
`NOT` inside `lex:`) apply to every stream; a flag from a pasted command (`--release`, `-s`,
`-1`) is searched for, not excluded. A small authority multiplier (0.85–1.15) from the
root weight and `pinned: true` breaks ties, and files matching a root's `demote` globs take a
further ×0.9 inside that band. A `MEMORY.md` index in a memory root never outranks a fact it
points to on a line matching the query. Results are one chunk per document with the other
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
gate. `tokens` is the mean size of what `recall` prints for the same queries. A line with
`"expected": []` is a question the corpus has no note for: it is left out of the hit rates
and counted under `quiet` when `recall` finds nothing or calls the match weak; `weak` is the
share of answerable queries wrongly treated that way. On a 38-query bench over the real
corpus: kvault hit@1 0.95, hit@5 1.00, MRR 0.97; grep hit@1 0.37, hit@5 0.87, MRR 0.57.

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

When none of the top three hits contains much of the query (under 45% of its words and
phrases, weighted so rare ones count more), the note asked for probably does not exist.
`recall` then prints a "Weak match" notice and up to five one-line references instead of
chunks about something else: a few hundred tokens instead of the whole budget. Nothing is
hidden, so an answer worded differently from the query is still listed and one `kvault get`
away. Single-word queries and `lex:` expressions are never treated as weak.

Shown text is verbatim except for trimming: frontmatter is dropped, trailing whitespace and
runs of blank lines are collapsed. Nothing inside a line is rewritten, so `path:line`
citations and code fences survive. A chunk whose file changed since it was indexed is
marked `[stale]` and shown as a preview at most, never quoted in full.

`kvault brief [--cwd P] [--budget N]` prints the context block the SessionStart hook injects
for a working directory: pinned notes and rules, notes updated in the last 14 days, and
notes elsewhere that link to or share tags with the project (the `session` tag aside), one
line each with a chunk id.
The project is the nearest ancestor directory whose name matches a configured root's
project. The block fits the budget (2000 tokens by default) and stays under 8,000 characters
so Claude Code does not swap it for a file preview; the header line is byte-stable across
runs except the counts and a coarse index age at its end.

## Claude Code integration

The repository is also a Claude Code plugin (`hooks/hooks.json`, `skills/kvault/SKILL.md`,
`.claude-plugin/plugin.json`) and its own single-plugin marketplace
(`.claude-plugin/marketplace.json`). With the binary on `PATH`, install it as shown in the
quick start, run one session with `claude --plugin-dir <clone>`, or copy the three hook
entries into `~/.claude/settings.json` and the skill directory into `~/.claude/skills/`.
The hooks:

| Event | Command | What it does |
|---|---|---|
| `SessionStart` (`startup`, `clear`, `compact`; not `resume` or `fork`) | `kvault hook session-start` | incremental index, then injects `kvault brief` for the session's `cwd` as `additionalContext`; logs the brief |
| `Stop` | `kvault hook stop` | writes or rewrites the session note, indexes it; logs which listed notes the session used |
| `PreCompact` | `kvault hook pre-compact` | same as Stop, so a compaction never loses the note |

Hooks read Claude Code's JSON on stdin, print JSON on stdout and always exit 0. A failure
prints `{}` and appends one line to `~/.local/state/kvault/hooks.log` (`KVAULT_LOG`
overrides), so a week of use can be audited with one `cat`. Stop and `PreCompact` make no
LLM calls and read about 1 MB of the transcript: the first 128 KB and the last 896 KB. (The
start is read further, up to 4 MB, only when the first prompt alone is larger than that, as
with a pasted image.) In a session longer than that, files touched in the unread middle stay
listed from the note's earlier versions.

The session note is `<project>/notes/sessions/<date>-<session8>.md`, one file per session
rewritten on every stop, where `<project>` is the nearest ancestor of `cwd` with a `.git`.
When the project has no `notes/` dir, or has one that no configured root covers (kvault
does not write into directories it does not index), it goes to `[capture] fallback_dir`
(default `~/.local/share/kvault/sessions/<project>/`). Each project directory there is indexed
without being configured, as an `other` root named for the project. The note carries
`kind: session`, the first prompt (its first line is the title, so the brief's recent
list reads as a log), the last assistant message, and the files the
transcript shows were read or edited. In the title, the prompt and the response,
`<private>…</private>` spans are dropped and credentials (PEM blocks, `Bearer x`, AWS,
GitHub, Slack, OpenAI, Anthropic and Google key prefixes, and values assigned to names
ending in `key`, `token`, `secret` or `password`) are replaced with `[redacted]`. The
list is short on purpose: treat it as a safety net, not a guarantee. Promoting a session note into a
durable note is the agent's job, through the skill and `kvault add`.

```
kvault add --title T [--kind decision|fact|howto|note] [--tags a,b] [--project P | --root DIR] [--file F]
```

writes `<slug>.md` with frontmatter into the project's notes root (the memory root when
that is the only one; `--root` to choose) and indexes it. The body comes from `--file` or
stdin. An existing file is replaced only when `--expected-hash` matches the BLAKE3 of its
current bytes (a prefix of eight or more hex digits is enough). `add` prints that hash
when it writes, and when it refuses to replace a note of the same title; a file whose
title differs (two titles can make the same file name) is never offered for replacement.
A new file is created only if its path is still free, so two agents cannot overwrite each
other unknowingly. In a Claude Code memory root the file uses that
loader's `name`/`description`/`metadata.type` frontmatter and a pointer line is appended to
`MEMORY.md`.

### Query log

`~/.local/state/kvault/queries.jsonl` (`KVAULT_QUERY_LOG` overrides; beside the index when
`KVAULT_DB` is set) gets one JSON line per event, each stamped with the kvault version:

| Line | Written by | Holds |
|---|---|---|
| `query` | `search`, `recall` | the query, working directory, session, results shown, time taken; for `recall` also tokens printed, the share of the query found and whether the reply was a weak match |
| `brief` | `SessionStart` hook | session, project, tokens, the notes listed, index size |
| `session` | first Stop or `PreCompact` of a session | that the session completed a turn |
| `use` | Stop, `PreCompact` | a note a query or the brief listed that the session then used: fetched with `kvault get`, touched by a file tool, named in a shell command, or named in the assistant's text |

It is the record of whether retrieval is helping. Nothing leaves the machine;
`[query_log] enabled = false` turns it off.

`kvault report [--since 7d] [--json]` summarises it: sessions given a brief, sessions that
searched, queries with a result used, notes from the brief that were followed up, weak
matches, tokens and latency, hook errors. The summary holds counts and rates only (no
query text, paths or project names), so it can be sent to someone else as it is, and
`--json` makes several people's reports easy to pool. `kvault report --full` prints the
log lines themselves, which do contain query text and file paths: read it before sharing.

## CLI

```
kvault init [--notes PATH]...                write config, index
kvault root add <path> [--kind K]            add a directory to the config, index
kvault index [--full] [--root P] [--prune]   incremental reindex
kvault search <q> [--project P] [--tag T] [--path G] [--since 30d] [--explain]
kvault recall <q> --budget N [--project P]   search packed to a token budget (agents call this)
kvault get <id|path>[:from[:count]]          print a chunk or line window
kvault brief [--cwd P] [--budget N]          what the SessionStart hook injects
kvault add --title T [--kind K] [--tags a,b] write a note and index it
kvault list [--project P] [--recent N]       indexed documents, newest first
kvault status                                roots, counts, absent roots, index age
kvault report [--since 7d] [--json|--full]   what the query log says about a period
kvault bench <queries.jsonl> [--baseline]    retrieval quality over a query file
kvault hook session-start|stop|pre-compact   Claude Code hook entry points
kvault links / serve                         not built yet (exit 3)
```

## License

MIT
