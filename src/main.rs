use std::io::Read;

use clap::Parser;
use kvault::cli::{Cli, Commands};
use kvault::commands;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Search {
            query,
            limit,
            category,
            case_sensitive,
        }) => {
            let results = commands::search(&query, limit, category, case_sensitive)?;

            if results.is_empty() {
                println!("No matches found for '{query}'");
                return Ok(());
            }

            for result in &results {
                println!(
                    "{}: {} (line {})",
                    result.title,
                    result.path.display(),
                    result.line_number
                );
                println!("  {}", result.matched_line);
            }

            println!("\n{} result(s) found", results.len());
            Ok(())
        }
        Some(Commands::List { category }) => {
            let documents = commands::list(category.as_deref())?;

            if documents.is_empty() {
                println!("No documents found.");
                return Ok(());
            }

            for doc in &documents {
                let tags = if doc.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", doc.tags.join(", "))
                };
                println!("{}: {}{tags}", doc.category, doc.title);
                println!("  {}", doc.path.display());
            }

            Ok(())
        }
        Some(Commands::Add {
            title,
            category,
            tags,
            file,
        }) => {
            let content = if let Some(path) = file {
                std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("Failed to read file {path}: {e}"))?
            } else {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            };

            if content.trim().is_empty() {
                anyhow::bail!("Content cannot be empty");
            }

            let tag_list = commands::parse_tags(tags);

            let result = commands::add(&title, &content, &category, tag_list)?;

            println!("Added: {}", result.title);
            println!("  Category: {}", result.category);
            println!("  Path: {}", result.path.display());

            Ok(())
        }
        Some(Commands::Get { path }) => {
            let content = commands::get(&path)?;
            print!("{content}");
            Ok(())
        }
        #[cfg(feature = "mcp")]
        Some(Commands::Serve) => tokio::runtime::Runtime::new()?.block_on(kvault::mcp::serve()),
        None => {
            Cli::parse_from(["kvault", "--help"]);
            Ok(())
        }
    }
}
