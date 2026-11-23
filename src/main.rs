use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use kvault::add::AddOptions;
use kvault::bench::BenchOptions;
use kvault::brief::BriefOptions;
use kvault::cli::{Cli, Command, KindArg, ProjectFromArg, RecallFormat, RootCommand, SearchArgs};
use kvault::config::{Config, ProjectFrom, RootConfig, contract_tilde, expand_tilde};
use kvault::error::{Error, Milestone};
use kvault::index::{self, IndexOptions};
use kvault::list::{self, ListFilter};
use kvault::querylog::{self, LoggedHit, NewQuery};
use kvault::recall::RecallOptions;
use kvault::roots::RootKind;
use kvault::search::{Query, SearchOptions};
use kvault::store::Store;
use kvault::{get, hooks, render, roots, update};

/// Write to stdout, ending the process quietly when the reader has gone
/// away (`kvault status | head`): `print!` would panic on the closed pipe.
fn emit(args: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    if let Err(e) = std::io::stdout().lock().write_fmt(args) {
        let code = i32::from(e.kind() != std::io::ErrorKind::BrokenPipe);
        std::process::exit(code);
    }
}

macro_rules! out {
    ($($arg:tt)*) => { emit(format_args!($($arg)*)) };
}

macro_rules! outln {
    ($($arg:tt)*) => { emit(format_args!("{}\n", format_args!($($arg)*))) };
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e @ Error::NotImplemented { .. }) => {
            // Distinct code so scripts can tell "not yet" from "failed".
            eprintln!("{e}");
            ExitCode::from(3)
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> kvault::Result<()> {
    match command {
        Command::Status => status(),
        Command::Init { force, notes } => init(force, &notes),
        Command::Root {
            command:
                RootCommand::Add {
                    path,
                    kind,
                    project_from,
                    weight,
                },
        } => root_add(&path, kind, project_from, weight),
        Command::Index {
            full,
            root,
            prune,
            quiet,
        } => {
            let opts = IndexOptions {
                full,
                quiet,
                only_root: root.as_deref().map(expand_tilde),
                prune,
            };
            run_index(&opts)
        }
        Command::List {
            root,
            kind,
            tag,
            project,
            recent,
            limit,
        } => list_docs(&ListFilter {
            root,
            kind,
            tag,
            project,
            recent_days: recent,
            limit,
        }),
        Command::Search(args) => search(&args),
        Command::Get { target, doc } => {
            let store = open_store()?;
            let out = get::run(&store, &get::Target::parse(&target), doc)?;
            out!("{out}");
            Ok(())
        }
        Command::Recall {
            query,
            budget,
            project,
            format,
        } => recall(&query, budget, project.as_deref(), format),
        Command::Brief { cwd, budget } => brief(cwd.as_deref(), budget),
        Command::Add {
            title,
            kind,
            project,
            tags,
            root,
            file,
            expected_hash,
        } => add(
            &AddOptions {
                title,
                kind,
                project,
                tags: tags
                    .map(|t| {
                        t.split(',')
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
                root: root.as_deref().map(expand_tilde),
                expected_hash,
                cwd: std::env::current_dir()?,
            },
            file.as_deref(),
        ),
        Command::Hook { event } => {
            outln!("{}", hooks::run(event));
            Ok(())
        }
        Command::Report { since, json, full } => report(since.as_deref(), json, full),
        Command::UpdateCheck { quiet } => update_check(quiet),
        Command::Links { .. } => not_yet("links", Milestone::M5Graph),
        Command::Bench {
            file,
            baseline,
            min_hit5,
            limit,
            verbose,
        } => bench(&file, baseline, min_hit5, limit, verbose),
        #[cfg(feature = "mcp")]
        Command::Serve => not_yet("serve", Milestone::M6Mcp),
    }
}

fn update_check(quiet: bool) -> kvault::Result<()> {
    let path = update::cache_path()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no state directory for the update check")))?;
    let latest = update::refresh(&path)?;
    if !quiet {
        let now = update::current();
        match latest {
            Some(v) if update::Version::parse(now).is_some_and(|n| v > n) => {
                outln!(
                    "kvault {v} is available (you have {now}): {}",
                    update::UPGRADE_COMMAND
                );
            }
            Some(v) => outln!("kvault {now} is up to date (latest release {v})"),
            None => outln!("kvault {now}: no releases found"),
        }
    }
    Ok(())
}

fn not_yet(command: &'static str, milestone: Milestone) -> kvault::Result<()> {
    Err(Error::NotImplemented { command, milestone })
}

fn db_path() -> kvault::Result<PathBuf> {
    Config::db_path().ok_or_else(|| Error::Other(anyhow::anyhow!("no data directory available")))
}

fn open_store() -> kvault::Result<Store> {
    Store::open(&db_path()?)
}

fn run_index(opts: &IndexOptions) -> kvault::Result<()> {
    let config = Config::load()?;
    if config.roots.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(
            "no roots configured; run `kvault init` or edit {}",
            Config::config_path().map_or("the config".into(), |p| p.display().to_string())
        )));
    }
    let mut store = open_store()?;
    let report = index::run(&config, &mut store, opts)?;
    if !opts.quiet {
        outln!("{report}");
    }
    Ok(())
}

/// A root for a notes path given on the command line: absolute, `~` kept
/// short, project naming worked out from its shape.
fn notes_root(arg: &str) -> kvault::Result<RootConfig> {
    let expanded = expand_tilde(arg);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    let path = contract_tilde(&absolute.components().collect::<PathBuf>());
    let project_from = ProjectFrom::infer(&path);
    Ok(RootConfig::new(path, RootKind::Notes, project_from))
}

/// `3 dirs: app, site, tools` for a root's expansion.
fn describe_matches(root: &RootConfig) -> String {
    let dirs = roots::expand(std::slice::from_ref(root), &BTreeMap::new());
    if dirs.is_empty() {
        return "nothing there yet".to_owned();
    }
    let mut projects: Vec<String> = dirs.iter().filter_map(|d| d.project.clone()).collect();
    projects.dedup();
    let named = match projects.len() {
        0 => String::new(),
        1..=4 => format!(": {}", projects.join(", ")),
        n => format!(": {}, … ({n} projects)", projects[..3].join(", ")),
    };
    format!(
        "{} dir{}{named}",
        dirs.len(),
        if dirs.len() == 1 { "" } else { "s" }
    )
}

fn init(force: bool, notes: &[String]) -> kvault::Result<()> {
    let path = Config::config_path()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no config directory available")))?;
    if path.exists() && !force {
        return Err(Error::Other(anyhow::anyhow!(
            "{} already exists; add a directory with `kvault root add <dir>`, \
             or pass --force to start over",
            path.display()
        )));
    }
    let mut config = Config::default();
    // Memory dirs appear as Claude Code is used, so the root is configured
    // whether or not any exist yet.
    let memory = Config::memory_root();
    outln!("add    {:<32} ({})", memory.path, describe_matches(&memory));
    config.roots.push(memory);
    let mut found_notes = false;
    for root in Config::suggested_notes_roots() {
        if roots::expand(std::slice::from_ref(&root), &BTreeMap::new()).is_empty() {
            outln!("skip   {:<32} (not found)", root.path);
        } else {
            outln!("add    {:<32} ({})", root.path, describe_matches(&root));
            config.roots.push(root);
            found_notes = true;
        }
    }
    for arg in notes {
        let root = notes_root(arg)?;
        if config.roots.iter().any(|r| r.path == root.path) {
            continue;
        }
        outln!("add    {:<32} ({})", root.path, describe_matches(&root));
        config.roots.push(root);
        found_notes = true;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, config.to_toml()?)?;
    outln!("wrote  {}", path.display());
    run_index(&IndexOptions::default())?;
    if !found_notes {
        outln!(
            "\nNo notes directories were found in the usual places. Add yours with\n  \
             kvault root add <dir>              one directory of notes\n  \
             kvault root add '~/code/*/notes'   one per project (quote the glob)"
        );
    }
    Ok(())
}

fn root_add(
    arg: &str,
    kind: KindArg,
    project_from: Option<ProjectFromArg>,
    weight: Option<f64>,
) -> kvault::Result<()> {
    let path = Config::config_path()
        .filter(|p| p.exists())
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no config yet; run `kvault init` first")))?;
    let mut root = notes_root(arg)?;
    root.kind = match kind {
        KindArg::Notes => RootKind::Notes,
        KindArg::Memory => RootKind::Memory,
        KindArg::Reference => RootKind::Reference,
        KindArg::Other => RootKind::Other,
    };
    if let Some(p) = project_from {
        root.project_from = match p {
            ProjectFromArg::Parent => ProjectFrom::Parent,
            ProjectFromArg::Dir => ProjectFrom::Dir,
            ProjectFromArg::ClaudeSlug => ProjectFrom::ClaudeSlug,
            ProjectFromArg::None => ProjectFrom::None,
        };
    }
    if let Some(w) = weight {
        root.weight = w;
    }
    Config::append_root(&path, &root)?;
    outln!(
        "added  {} ({}, project from {}; {})",
        root.path,
        root.kind.as_str(),
        root.project_from.as_str(),
        describe_matches(&root)
    );
    if roots::expand(std::slice::from_ref(&root), &BTreeMap::new()).is_empty() {
        outln!("       nothing matches yet; it will be indexed once it exists");
    }
    run_index(&IndexOptions::default())
}

fn search(args: &SearchArgs) -> kvault::Result<()> {
    let store = open_store()?;
    let query = Query::parse(&args.query);
    let opts = SearchOptions {
        limit: args.limit,
        project: args.project.clone(),
        root: args.root.as_deref().map(expand_tilde),
        kind: args.kind.clone(),
        tags: args.tag.clone(),
        path: args.path.clone(),
        since: args.since.clone(),
        per_chunk: args.per_chunk,
    };
    let started = std::time::Instant::now();
    let result = kvault::search::run(&store, &query, &opts)?;
    let elapsed = started.elapsed();
    if let Ok(config) = Config::load() {
        querylog::record(
            &config,
            NewQuery {
                command: "search",
                query: &args.query,
                project: args.project.as_deref(),
                budget: None,
                total: result.candidates,
                weak: false,
                coverage: None,
                tokens: None,
                elapsed,
                hits: result
                    .hits
                    .iter()
                    .map(|h| LoggedHit {
                        id: h.chunk.id.clone(),
                        path: h.display_path.clone(),
                        file: h.doc().abs_path(),
                    })
                    .collect(),
            },
        );
    }
    out!("{}", render::render(&result, args.format, args.explain)?);
    Ok(())
}

fn recall(
    query: &str,
    budget: usize,
    project: Option<&str>,
    format: RecallFormat,
) -> kvault::Result<()> {
    let store = open_store()?;
    let raw = query;
    let query = Query::parse(raw);
    let started = std::time::Instant::now();
    let result = kvault::recall::run(
        &store,
        &query,
        &RecallOptions {
            budget,
            project: project.map(str::to_owned),
            format,
        },
    )?;
    let elapsed = started.elapsed();
    if let Ok(config) = Config::load() {
        querylog::record(
            &config,
            NewQuery {
                command: "recall",
                query: raw,
                project,
                budget: Some(budget),
                total: result.total,
                weak: result.weak,
                coverage: result.coverage,
                tokens: Some(result.tokens),
                elapsed,
                hits: result
                    .entries
                    .iter()
                    .map(|e| LoggedHit {
                        id: e.id.clone(),
                        path: e.path.clone(),
                        file: e.abs_path.clone(),
                    })
                    .collect(),
            },
        );
    }
    if result.text.is_empty() {
        eprintln!("budget {budget} is too small for any output");
    }
    out!("{}", result.text);
    Ok(())
}

fn add(opts: &AddOptions, file: Option<&str>) -> kvault::Result<()> {
    let body = if let Some(f) = file {
        std::fs::read_to_string(expand_tilde(f))?
    } else {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)?;
        s
    };
    let config = Config::load()?;
    let mut store = open_store()?;
    let added = kvault::add::run(&config, &mut store, opts, &body)?;
    outln!(
        "{}  {}",
        if added.replaced {
            "replaced"
        } else {
            "wrote   "
        },
        added.path.display()
    );
    outln!("blake3    {}", added.hash);
    if !added.indexed {
        outln!("not indexed: the file is outside every configured root");
    }
    Ok(())
}

fn brief(cwd: Option<&str>, budget: Option<usize>) -> kvault::Result<()> {
    let config = Config::load()?;
    let store = open_store()?;
    let cwd = match cwd {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let opts = BriefOptions {
        cwd,
        budget: budget.unwrap_or(config.brief.budget),
        recent_days: config.brief.recent_days,
    };
    out!("{}", kvault::brief::run(&store, &opts)?);
    Ok(())
}

fn bench(
    file: &str,
    baseline: bool,
    min_hit5: Option<f64>,
    limit: usize,
    verbose: bool,
) -> kvault::Result<()> {
    let store = open_store()?;
    let cases = kvault::bench::load(&expand_tilde(file))?;
    let report = kvault::bench::run(&store, &cases, &BenchOptions { limit, baseline })?;
    out!("{}", report.render(verbose));
    if let Some(min) = min_hit5
        && report.kvault.metrics.hit5 < min
    {
        return Err(Error::Other(anyhow::anyhow!(
            "hit@5 {:.2} is below the required {min:.2}",
            report.kvault.metrics.hit5
        )));
    }
    Ok(())
}

fn report(since: Option<&str>, json: bool, full: bool) -> kvault::Result<()> {
    let since = match since {
        Some(s) => Some(
            kvault::search::parse_since(s)
                .ok_or_else(|| Error::Other(anyhow::anyhow!("cannot parse --since {s:?}")))?,
        ),
        None => None,
    };
    let Some(log) = Config::query_log_path() else {
        return Err(Error::Other(anyhow::anyhow!(
            "no state directory available"
        )));
    };
    let events = kvault::report::load(&log, since.as_deref())?;
    if full {
        out!("{}", kvault::report::full(&events)?);
        return Ok(());
    }
    let errors =
        Config::hook_log_path().map_or(0, |p| kvault::report::hook_errors(&p, since.as_deref()));
    let summary = kvault::report::summarize(&events, errors);
    if json {
        let text = serde_json::to_string_pretty(&summary).map_err(|e| Error::Other(e.into()))?;
        outln!("{text}");
    } else {
        out!("{}", summary.render());
    }
    Ok(())
}

fn list_docs(filter: &ListFilter) -> kvault::Result<()> {
    let store = open_store()?;
    let docs = list::run(&store, filter)?;
    if docs.is_empty() {
        outln!("no documents (run `kvault index`?)");
        return Ok(());
    }
    for d in &docs {
        let date = d
            .updated
            .as_deref()
            .map_or("-", |u| u.get(..10).unwrap_or(u));
        outln!(
            "{:<9} {:<14} {date}  {:<6} {}  {}",
            d.root_kind,
            d.project.as_deref().unwrap_or("-"),
            d.chunks,
            d.rel_path,
            d.title
        );
    }
    outln!("{} documents", docs.len());
    Ok(())
}

fn status() -> kvault::Result<()> {
    let config = Config::load()?;
    let config_path = Config::config_path();

    outln!(
        "config:  {}{}",
        config_path
            .as_deref()
            .map_or("<none>".into(), |p| p.display().to_string()),
        if config_path.as_deref().is_some_and(std::path::Path::exists) {
            ""
        } else {
            "  (not found, using defaults)"
        }
    );

    outln!("version: {}", update::status_line(&config));

    let expanded = roots::expand_config(&config);
    outln!(
        "roots:   {} configured, {} present",
        config.roots.len(),
        expanded.len()
    );
    for r in &expanded {
        outln!(
            "  {:<9} {:<4.1} {:<12} {}",
            r.kind.as_str(),
            r.weight,
            r.project.as_deref().unwrap_or("-"),
            r.path.display()
        );
    }
    if config.roots.is_empty() {
        outln!("  (none configured; run `kvault init`)");
    }

    let p = db_path()?;
    if p.exists() {
        let store = Store::open(&p)?;
        let c = store.counts()?;
        let size = std::fs::metadata(&p).map_or(0, |m| m.len());
        outln!("index:   {} ({} KB)", p.display(), size / 1024);
        outln!(
            "         {} roots, {} docs, {} chunks, {} links, {} tags",
            c.roots,
            c.docs,
            c.chunks,
            c.links,
            c.tags
        );
        for (path, docs) in store.absent_roots()? {
            outln!(
                "         absent: {path} ({docs} docs hidden; `kvault index --prune` drops them)"
            );
        }
        if let Some(t) = store.meta("last_index_at")? {
            outln!("         last indexed {t}");
        }
    } else {
        outln!("index:   {} (not built; run `kvault index`)", p.display());
    }
    Ok(())
}
