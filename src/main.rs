use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use kvault::cli::{Cli, Command};
use kvault::config::{Config, expand_tilde};
use kvault::error::{Error, Milestone};
use kvault::index::{self, IndexOptions};
use kvault::list::{self, ListFilter};
use kvault::roots;
use kvault::store::Store;

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
        Command::Init { force } => init(force),
        Command::Index { full, root, quiet } => {
            let opts = IndexOptions {
                full,
                quiet,
                only_root: root.as_deref().map(expand_tilde),
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
        Command::Search(args) => kvault::search::run(&args.query).map(|_| ()),
        Command::Get { .. } => not_yet("get", Milestone::M2Search),
        Command::Recall { .. } => not_yet("recall", Milestone::M3Recall),
        Command::Brief { .. } => not_yet("brief", Milestone::M3Recall),
        Command::Add { .. } => not_yet("add", Milestone::M4Hooks),
        Command::Hook { .. } => not_yet("hook", Milestone::M4Hooks),
        Command::Links { .. } => not_yet("links", Milestone::M5Graph),
        Command::Bench { .. } => not_yet("bench", Milestone::M2Search),
        #[cfg(feature = "mcp")]
        Command::Serve => not_yet("serve", Milestone::M6Mcp),
    }
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
        println!("{report}");
    }
    Ok(())
}

fn init(force: bool) -> kvault::Result<()> {
    let path = Config::config_path()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no config directory available")))?;
    if path.exists() && !force {
        return Err(Error::Other(anyhow::anyhow!(
            "{} already exists; pass --force to overwrite",
            path.display()
        )));
    }
    let mut config = Config::default();
    for root in Config::suggested_roots() {
        let present = roots::expand(std::slice::from_ref(&root));
        if present.is_empty() {
            println!("skip   {:<32} (no matching directories)", root.path);
        } else {
            println!("add    {:<32} ({} dirs)", root.path, present.len());
            config.roots.push(root);
        }
    }
    if config.roots.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(
            "none of the suggested roots exist; write {} by hand",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, config.to_toml()?)?;
    println!("wrote  {}", path.display());
    run_index(&IndexOptions::default())
}

fn list_docs(filter: &ListFilter) -> kvault::Result<()> {
    let store = open_store()?;
    let docs = list::run(&store, filter)?;
    if docs.is_empty() {
        println!("no documents (run `kvault index`?)");
        return Ok(());
    }
    for d in &docs {
        let date = d
            .updated
            .as_deref()
            .map_or("-", |u| u.get(..10).unwrap_or(u));
        println!(
            "{:<9} {:<14} {date}  {:<6} {}  {}",
            d.root_kind,
            d.project.as_deref().unwrap_or("-"),
            d.chunks,
            d.rel_path,
            d.title
        );
    }
    println!("{} documents", docs.len());
    Ok(())
}

fn status() -> kvault::Result<()> {
    let config = Config::load()?;
    let config_path = Config::config_path();

    println!(
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

    let expanded = roots::expand(&config.roots);
    println!(
        "roots:   {} configured, {} present",
        config.roots.len(),
        expanded.len()
    );
    for r in &expanded {
        println!(
            "  {:<9} {:<4.1} {:<12} {}",
            r.kind.as_str(),
            r.weight,
            r.project.as_deref().unwrap_or("-"),
            r.path.display()
        );
    }
    if config.roots.is_empty() {
        println!("  (none configured; run `kvault init`)");
    }

    let p = db_path()?;
    if p.exists() {
        let store = Store::open(&p)?;
        let c = store.counts()?;
        let size = std::fs::metadata(&p).map_or(0, |m| m.len());
        println!("index:   {} ({} KB)", p.display(), size / 1024);
        println!(
            "         {} roots, {} docs, {} chunks, {} links, {} tags",
            c.roots, c.docs, c.chunks, c.links, c.tags
        );
        if let Some(t) = store.meta("last_index_at")? {
            println!("         last indexed {t}");
        }
    } else {
        println!("index:   {} (not built; run `kvault index`)", p.display());
    }
    Ok(())
}
