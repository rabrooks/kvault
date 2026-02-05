use clap::Parser;
use kvault::cli::{Cli, Commands};
use kvault::config::{Config, expand_tilde};
use kvault::corpus::Corpus;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Search {
            query,
            limit,
            category,
            scope,
        }) => {
            println!(
                "Searching for '{query}' (limit: {limit}, category: {category:?}, scope: {scope})"
            );
            todo!("Implement search in Phase 3")
        }
        Some(Commands::List { category, scope }) => list_documents(category, &scope),
        Some(Commands::Add {
            title,
            category,
            tags,
            scope,
            file,
        }) => {
            println!(
                "Adding document '{title}' (category: {category}, tags: {tags:?}, scope: {scope}, file: {file:?})"
            );
            todo!("Implement add in Phase 5")
        }
        Some(Commands::Get { path }) => get_document(&path),
        #[cfg(feature = "mcp")]
        Some(Commands::Serve) => tokio::runtime::Runtime::new()?.block_on(kvault::mcp::serve()),
        None => {
            Cli::parse_from(["kvault", "--help"]);
            Ok(())
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // Will refactor when implementing scope
fn list_documents(category: Option<String>, _scope: &str) -> anyhow::Result<()> {
    let config = Config::load()?;

    let mut found_any = false;

    for path_str in &config.corpus.paths {
        let path = expand_tilde(path_str);

        if !path.exists() {
            continue;
        }

        match Corpus::load(&path) {
            Ok(corpus) => {
                for doc in corpus.documents() {
                    if let Some(ref cat) = category
                        && &doc.category != cat
                    {
                        continue;
                    }

                    found_any = true;
                    let tags = if doc.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", doc.tags.join(", "))
                    };
                    println!("{}: {}{tags}", doc.category, doc.title);
                    println!("  {}", corpus.resolve_document_path(doc).display());
                }
            }
            Err(e) => {
                eprintln!("Warning: Could not load corpus at {}: {e}", path.display());
            }
        }
    }

    if !found_any {
        println!("No documents found.");
        println!("Searched paths:");
        for path_str in &config.corpus.paths {
            let path = expand_tilde(path_str);
            let status = if path.exists() { "exists" } else { "not found" };
            println!("  {} ({status})", path.display());
        }
    }

    Ok(())
}

fn get_document(doc_path: &str) -> anyhow::Result<()> {
    let config = Config::load()?;

    for path_str in &config.corpus.paths {
        let corpus_path = expand_tilde(path_str);

        if !corpus_path.exists() {
            continue;
        }

        if let Ok(corpus) = Corpus::load(&corpus_path) {
            for doc in corpus.documents() {
                if doc.path.to_string_lossy() == doc_path {
                    let full_path = corpus.resolve_document_path(doc);
                    let content = std::fs::read_to_string(&full_path)?;
                    print!("{content}");
                    return Ok(());
                }
            }
        }
    }

    anyhow::bail!("Document not found: {doc_path}")
}
