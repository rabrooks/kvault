//! MCP server implementation for kvault.
//!
//! Exposes kvault functionality as MCP tools for AI editors.

use std::borrow::Cow;
use std::fmt::Write;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorCode, ErrorData as McpError, ServerCapabilities, ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;

use crate::cli::DEFAULT_SEARCH_LIMIT;
use crate::commands;

/// Parameters for `search_knowledge` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    #[schemars(description = "The search query")]
    pub query: String,
    #[schemars(description = "Maximum number of results (default: 10)")]
    pub limit: Option<usize>,
    #[schemars(description = "Filter by category")]
    pub category: Option<String>,
    #[schemars(description = "Use case-sensitive matching (default: false)")]
    pub case_sensitive: Option<bool>,
}

/// Parameters for `list_knowledge` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListParams {
    #[schemars(description = "Filter by category")]
    pub category: Option<String>,
}

/// Parameters for `get_document` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetParams {
    #[schemars(description = "Document path (e.g., 'aws/lambda-patterns.md')")]
    pub path: String,
}

/// Parameters for `add_knowledge` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddParams {
    #[schemars(description = "Document title")]
    pub title: String,
    #[schemars(description = "Document content (markdown)")]
    pub content: String,
    #[schemars(description = "Category for grouping (e.g., 'aws', 'rust')")]
    pub category: String,
    #[schemars(description = "Comma-separated tags")]
    pub tags: Option<String>,
}

/// MCP server exposing kvault tools.
#[derive(Clone)]
pub struct KvaultServer {
    tool_router: ToolRouter<Self>,
}

impl Default for KvaultServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl KvaultServer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Search the knowledge corpus for documents matching a query")]
    async fn search_knowledge(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        let case_sensitive = params.case_sensitive.unwrap_or(false);

        match commands::search(&params.query, limit, params.category, case_sensitive) {
            Ok(results) => {
                if results.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "No matches found for '{}'",
                        params.query
                    ))]));
                }

                let mut output = String::new();
                for result in &results {
                    let _ = write!(
                        output,
                        "## {}\n**File:** {}\n**Line {}:** {}\n\n",
                        result.title,
                        result.path.display(),
                        result.line_number,
                        result.matched_line
                    );
                }
                let _ = write!(output, "*{} result(s) found*", results.len());

                Ok(CallToolResult::success(vec![Content::text(output)]))
            }
            Err(e) => Err(McpError {
                code: ErrorCode::INTERNAL_ERROR,
                message: Cow::from(format!("Search failed: {e}")),
                data: None,
            }),
        }
    }

    #[tool(description = "List all documents in the knowledge corpus")]
    async fn list_knowledge(
        &self,
        Parameters(params): Parameters<ListParams>,
    ) -> Result<CallToolResult, McpError> {
        match commands::list(params.category.as_deref()) {
            Ok(documents) => {
                if documents.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "No documents found.".to_string(),
                    )]));
                }

                let mut output = String::new();
                for doc in &documents {
                    let tags = if doc.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", doc.tags.join(", "))
                    };
                    let _ = write!(
                        output,
                        "- **{}**: {}{}\n  `{}`\n",
                        doc.category,
                        doc.title,
                        tags,
                        doc.path.display()
                    );
                }

                Ok(CallToolResult::success(vec![Content::text(output)]))
            }
            Err(e) => Err(McpError {
                code: ErrorCode::INTERNAL_ERROR,
                message: Cow::from(format!("List failed: {e}")),
                data: None,
            }),
        }
    }

    #[tool(description = "Get the full contents of a document by its path")]
    async fn get_document(
        &self,
        Parameters(params): Parameters<GetParams>,
    ) -> Result<CallToolResult, McpError> {
        match commands::get(&params.path) {
            Ok(content) => Ok(CallToolResult::success(vec![Content::text(content)])),
            Err(e) => Err(McpError {
                code: ErrorCode::INTERNAL_ERROR,
                message: Cow::from(format!("Failed to get document: {e}")),
                data: None,
            }),
        }
    }

    #[tool(description = "Add a new document to the knowledge corpus")]
    async fn add_knowledge(
        &self,
        Parameters(params): Parameters<AddParams>,
    ) -> Result<CallToolResult, McpError> {
        let tag_list = commands::parse_tags(params.tags);

        match commands::add(&params.title, &params.content, &params.category, tag_list) {
            Ok(result) => {
                let output = format!(
                    "Added document:\n- **Title:** {}\n- **Category:** {}\n- **Path:** {}",
                    result.title,
                    result.category,
                    result.path.display()
                );
                Ok(CallToolResult::success(vec![Content::text(output)]))
            }
            Err(e) => Err(McpError {
                code: ErrorCode::INTERNAL_ERROR,
                message: Cow::from(format!("Failed to add document: {e}")),
                data: None,
            }),
        }
    }
}

#[tool_handler]
impl ServerHandler for KvaultServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "kvault provides searchable access to a knowledge corpus. \
                Use search_knowledge to find documents, list_knowledge to browse, \
                get_document to read full contents, and add_knowledge to save new documents."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// Start the MCP server with stdio transport.
///
/// # Errors
///
/// Returns an error if the server fails to start or encounters a fatal error.
pub async fn serve() -> anyhow::Result<()> {
    let server = KvaultServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
