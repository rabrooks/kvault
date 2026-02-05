use clap::Parser;
use kvault::cli::{Cli, Commands};
use kvault::config::{expand_tilde, Config};
use kvault::corpus::Corpus;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Search { query, limit, category, scope }) => {
            println!(
                "Searching for '{}' (limit: {}, category: {:?}, scope: {})",
                query, limit, category, scope
            );
            todo!("Implement search in Phase 3")
        }
        Some(Commands::List { category, scope }) => {
            list_documents(category, scope)
        }
        Some(Commands::Add { title, category, tags, scope, file }) => {
            println!(
                "Adding document '{}' (category: {}, tags: {:?}, scope: {}, file: {:?})",
                title, category, tags, scope, file
            );
            todo!("Implement add in Phase 5")
        }
        Some(Commands::Get { path }) => {
            get_document(&path)
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

fn list_documents(category: Option<String>, _scope: String) -> anyhow::Result<()> {
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
                    if let Some(ref cat) = category {
                        if &doc.category != cat {
                            continue;
                        }
                    }

                    found_any = true;
                    let tags = if doc.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", doc.tags.join(", "))
                    };
                    println!("{}: {}{}", doc.category, doc.title, tags);
                    println!("  {}", corpus.resolve_document_path(doc).display());
                }
            }
            Err(e) => {
                eprintln!("Warning: Could not load corpus at {}: {}", path.display(), e);
            }
        }
    }

    if !found_any {
        println!("No documents found.");
        println!("Searched paths:");
        for path_str in &config.corpus.paths {
            let path = expand_tilde(path_str);
            let status = if path.exists() { "exists" } else { "not found" };
            println!("  {} ({})", path.display(), status);
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
                    print!("{}", content);
                    return Ok(());
                }
            }
        }
    }

    anyhow::bail!("Document not found: {}", doc_path)
}
