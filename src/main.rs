use std::process::ExitCode;

use clap::Parser;

use kvault::cli::{Cli, Command};
use kvault::config::Config;
use kvault::error::{Error, Milestone};
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
        Command::Index { full, quiet, .. } => {
            kvault::index::run(kvault::index::IndexOptions { full, quiet })
        }
        Command::Search(args) => kvault::search::run(&args.query).map(|_| ()),
        Command::Init { .. } => not_yet("init", Milestone::M1Index),
        Command::List { .. } => not_yet("list", Milestone::M1Index),
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

fn status() -> kvault::Result<()> {
    let config = Config::load()?;
    let config_path = Config::config_path();
    let db_path = Config::db_path();

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

    match db_path {
        Some(p) if p.exists() => {
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
        }
        Some(p) => println!("index:   {} (not built; run `kvault index`)", p.display()),
        None => println!("index:   <no data dir available>"),
    }
    Ok(())
}
