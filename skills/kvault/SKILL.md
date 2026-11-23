---
name: kvault
description: Search and save long-term notes and memories across projects. Use before answering "what did we decide", "have I seen this before", or "where is X documented", and to save a durable decision.
allowed-tools: Bash(kvault *)
---

# kvault

Markdown notes on disk, indexed in place. The session brief you were given at start
is the passive view; these commands are the active one.

## Search before claiming ignorance

```
kvault recall "<query>" --budget 1500 [--project <p>]
```

You write the query; kvault does no expansion. Use the vocabulary the note would
use, not the question's:

- bare words match any, prefix-matched: `lambda cold start`
- quotes make a phrase, `-word` excludes: `"cold start" -timeout`
  (flags in a pasted command, `--release` or `-s`, are searched for, not excluded)
- `tag:devops` filters; `tag:x` alone lists tagged notes newest first
- `--budget` is tokens of output; 600 for a quick check, 1500 by default, 3000 when
  the answer is probably long

A reply that starts with "Weak match" means no note contains much of your query and
only references are listed. Usually there is no note on it: say so, or try once more
with the note's likely vocabulary. If a listed title looks right, `kvault get` it.

Each hit shows an id, a path with line numbers, and either the chunk or a preview.
`kvault search <q> --explain` shows why a hit ranked where it did when results look
wrong.

## Read before quoting

```
kvault get <id>                 the chunk, with line numbers
kvault get <path>:<from>:<n>    a line window
kvault get <path> --doc         the whole file
```

A `[stale]` mark means the file changed since it was indexed: read it, do not quote
the preview. Cite `path:line` in answers.

## Save what should outlive the session

```
kvault add --title "<topic>" --kind decision|fact|howto|note [--tags a,b] [--project <p>] --file body.md
printf '%s\n' "<body>" | kvault add --title "<topic>" --kind decision
```

Writes `<slug>.md` into the project's notes root (the memory root when that is the
only one) and indexes it. One note per topic: to update, `kvault get` it, then run
`add` with the same title and `--expected-hash <blake3>`. `add` prints the blake3
when it writes, and again when it refuses to replace a note of the same title; a
mismatch means someone else wrote first, so read again. If `add` says the file name
is taken by a different note, pick a more specific title. Save decisions with the why; facts and how-tos as
one line of claim then detail. Session notes (`kind: session`) are written
automatically on every stop; promote anything durable out of them with `add`, do
not edit them.

## Housekeeping

- `kvault index` after bulk edits outside Claude Code; hooks run it otherwise.
- `kvault list --project <p> --recent 7` to see what changed lately.
- `kvault status` when results look empty.
