use clap::Parser;
use kvault::cli::{Cli, Commands};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Search { query, limit, category, scope }) => {
            println!("Searching for '{}' (limit: {}, category: {:?}, scope: {})",
                     query, limit, category, scope);
            todo!("Implement search in Phase 3")
        }
        Some(Commands::List { category, scope }) => {
            println!("Listing documents (category: {:?}, scope: {})", category, scope);
            todo!("Implement list in Phase 2")
        }
        Some(Commands::Add { title, category, tags, scope, file }) => {
            println!("Adding document '{}' (category: {}, tags: {:?}, scope: {}, file: {:?})",
                     title, category, tags, scope, file);
            todo!("Implement add in Phase 5")
        }
        Some(Commands::Get { path }) => {
            println!("Getting document at '{}'", path);
            todo!("Implement get in Phase 2")
        }
        #[cfg(feature = "mcp")]
        Some(Commands::Serve) => {
            tokio::runtime::Runtime::new()?.block_on(kvault::mcp::serve())
        }
        None => {
            Cli::parse_from(["kvault", "--help"]);
            Ok(())
        }
    }
}
