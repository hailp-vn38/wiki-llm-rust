use crate::db::filter_sql::MetadataFilter;
use crate::error::{WikiError, WikiResult};
use crate::fs::file_ops::{sanitize_mermaid_fences, slice_lines_with_budget, AdaptiveReadBudget};
use crate::fs::tree::{TreeOptions, TreeScope};
use crate::search::metadata::MetadataDiscoveryRequest;
use crate::search::{HybridQueryRequest, SubQuery};
use crate::types::{CollectionScope, GetOptions, MultiGetOptions};
use crate::wiki::ingest::{FileSynthesisRequest, IngestSourceRequest};
use crate::wiki::log_md::LogAction;
use crate::WikiEngine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Compact, token-optimized outcome returned by `execute_tool_call_with_budget`
/// for LLM agent/subagent tool loops (with adaptive line-bounded reading, compact `wiki/index.md`,
/// automatic Mermaid fence sanitization, and human-readable progress messages).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolOutcome {
    /// Formatted text content to return directly to the LLM as the tool result.
    pub content: String,
    /// Structured JSON metadata associated with the tool execution.
    pub metadata: Value,
    /// Optional human-readable progress message (e.g. for agent event streams).
    pub progress_message: Option<String>,
}

/// Execute one of the 13 LLM tools by name with JSON arguments and return a structured JSON response.
pub fn execute_tool_call(engine: &WikiEngine, tool_name: &str, mut args: Value) -> WikiResult<Value> {
    if let Some(obj) = args.as_object_mut() {
        for field in [
            "wiki_markdown_body",
            "content",
            "append_text",
            "replacement_text",
            "markdown_body",
        ] {
            if let Some(Value::String(s)) = obj.get_mut(field) {
                if s.contains("</mermaid>") {
                    *s = sanitize_mermaid_fences(s);
                }
            }
        }
    }

    match tool_name {
        "wiki_tree" => {
            let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("all");
            let scope = match scope_str.to_ascii_lowercase().as_str() {
                "sources" | "source" => TreeScope::Sources,
                "wiki" => TreeScope::Wiki,
                _ => TreeScope::All,
            };
            let sub_path = args
                .get("sub_path")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let include_metadata = args
                .get("include_metadata")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let include_non_markdown = args
                .get("include_non_markdown")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let tree = engine.get_tree(TreeOptions {
                scope,
                sub_path,
                max_depth,
                include_metadata,
                include_non_markdown,
            })?;
            let ascii = tree.to_ascii_tree();
            Ok(json!({
                "ascii_tree": ascii,
                "total_files": tree.total_files(),
                "total_directories": tree.total_directories(),
                "tree": tree,
            }))
        }

        "wiki_query" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            let searches: Vec<SubQuery> = match args.get("searches") {
                Some(v) if !v.is_null() => serde_json::from_value(v.clone()).map_err(|e| {
                    WikiError::InvalidToolArguments {
                        tool: tool_name.to_string(),
                        message: format!("Invalid `searches`: {e}"),
                    }
                })?,
                _ => Vec::new(),
            };
            let collections = parse_collections_arg(args.get("collections"));
            let filter: Option<MetadataFilter> = match args.get("filter") {
                Some(v) if !v.is_null() => {
                    Some(serde_json::from_value(v.clone()).map_err(|e| {
                        WikiError::InvalidToolArguments {
                            tool: tool_name.to_string(),
                            message: format!("Invalid `filter`: {e}"),
                        }
                    })?)
                }
                _ => None,
            };
            let intent = args
                .get("intent")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(10);
            let min_score = args
                .get("min_score")
                .or_else(|| args.get("minScore"))
                .and_then(|v| v.as_f64())
                .map(|f| f as f32)
                .unwrap_or(0.0);
            let candidate_limit = args
                .get("candidate_limit")
                .or_else(|| args.get("candidateLimit"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let rerank = args.get("rerank").and_then(|v| v.as_bool()).unwrap_or(true);
            let explain = args
                .get("explain")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let results = engine.query(HybridQueryRequest {
                query,
                searches,
                collections,
                filter,
                intent,
                limit,
                min_score,
                candidate_limit,
                rerank,
                explain,
            })?;

            Ok(json!({
                "count": results.len(),
                "results": results,
            }))
        }

        "wiki_get" => {
            let file = args.get("file").and_then(|v| v.as_str()).ok_or_else(|| {
                WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: "Missing required string parameter `file`".to_string(),
                }
            })?;
            let from_line = args
                .get("from_line")
                .or_else(|| args.get("fromLine"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let max_lines = args
                .get("max_lines")
                .or_else(|| args.get("maxLines"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let line_numbers = args
                .get("line_numbers")
                .or_else(|| args.get("lineNumbers"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let doc = engine.get_document(
                file,
                GetOptions {
                    from_line,
                    max_lines,
                    line_numbers,
                    include_frontmatter: true,
                },
            )?;
            Ok(serde_json::to_value(doc)?)
        }

        "wiki_multi_get" => {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: "Missing required string parameter `pattern`".to_string(),
                })?;
            let max_bytes = args
                .get("max_bytes")
                .or_else(|| args.get("maxBytes"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(65_536);
            let max_lines = args
                .get("max_lines")
                .or_else(|| args.get("maxLines"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let line_numbers = args
                .get("line_numbers")
                .or_else(|| args.get("lineNumbers"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let res = engine.multi_get(
                pattern,
                MultiGetOptions {
                    max_bytes,
                    max_lines,
                    line_numbers,
                },
            )?;
            Ok(serde_json::to_value(res)?)
        }

        "wiki_metadata" => {
            let collections = parse_collections_arg(args.get("collections"));
            let match_cond: Option<MetadataFilter> = match args.get("match") {
                Some(v) if !v.is_null() => {
                    Some(serde_json::from_value(v.clone()).map_err(|e| {
                        WikiError::InvalidToolArguments {
                            tool: tool_name.to_string(),
                            message: format!("Invalid `match`: {e}"),
                        }
                    })?)
                }
                _ => None,
            };
            let filter: Option<MetadataFilter> = match args.get("filter") {
                Some(v) if !v.is_null() => {
                    Some(serde_json::from_value(v.clone()).map_err(|e| {
                        WikiError::InvalidToolArguments {
                            tool: tool_name.to_string(),
                            message: format!("Invalid `filter`: {e}"),
                        }
                    })?)
                }
                _ => None,
            };
            let key_limit = args
                .get("key_limit")
                .or_else(|| args.get("keyLimit"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let value_limit = args
                .get("value_limit")
                .or_else(|| args.get("valueLimit"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let min_count = args
                .get("min_count")
                .or_else(|| args.get("minCount"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            let res = engine.discover_metadata(MetadataDiscoveryRequest {
                collections,
                match_cond,
                filter,
                key_limit,
                key_offset: None,
                value_limit,
                value_offset: None,
                sort: Default::default(),
                min_count,
            })?;
            Ok(serde_json::to_value(res)?)
        }

        "wiki_status" => {
            let status = engine.status()?;
            Ok(serde_json::to_value(status)?)
        }

        "wiki_write_file" => {
            let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("wiki");
            let scope = CollectionScope::from_str_opt(scope_str).ok_or_else(|| {
                WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: format!("Invalid scope '{scope_str}', expected 'wiki' or 'sources'"),
                }
            })?;
            let rel_path = args
                .get("rel_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: "Missing required string parameter `rel_path`".to_string(),
                })?;
            let prefix = format!("{}/", scope.as_str());
            let clean_rel = rel_path.trim_start_matches('/');
            let clean_scoped = clean_rel.strip_prefix(&prefix).unwrap_or(clean_rel);
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: "Missing required string parameter `content`".to_string(),
                })?;
            let update_index = args
                .get("update_index_md")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let info = engine.write_file(scope, clean_scoped, content)?;
            if scope == CollectionScope::Wiki && update_index {
                let _ = engine.rebuild_or_sync_index_md();
            }
            Ok(serde_json::to_value(info)?)
        }

        "wiki_patch_file" => {
            let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("wiki");
            let scope = CollectionScope::from_str_opt(scope_str).ok_or_else(|| {
                WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: format!("Invalid scope '{scope_str}'"),
                }
            })?;
            let rel_path = args
                .get("rel_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: "Missing `rel_path`".to_string(),
                })?;

            let prefix = format!("{}/", scope.as_str());
            let clean_rel = rel_path.trim_start_matches('/');
            let clean_scoped = clean_rel.strip_prefix(&prefix).unwrap_or(clean_rel);
            let existing = engine.get_document(
                &format!("{}/{clean_scoped}", scope.as_str()),
                GetOptions::default(),
            )?;

            let mut updated = existing.content;
            if let (Some(target), Some(replacement)) = (
                args.get("target_text").and_then(|v| v.as_str()),
                args.get("replacement_text").and_then(|v| v.as_str()),
            ) {
                if !target.is_empty() {
                    if updated.contains(target) {
                        updated = updated.replace(target, replacement);
                    } else {
                        // Also allow matching if the caller omitted the surrounding `[[` and `]]`
                        let wrapped = format!("[[{}]]", target.trim_start_matches("[[").trim_end_matches("]]"));
                        if updated.contains(&wrapped) {
                            let clean_rep = if replacement.trim().starts_with("[[") && replacement.trim().ends_with("]]") {
                                replacement.trim().to_string()
                            } else {
                                format!("[[{}]]", replacement.trim())
                            };
                            updated = updated.replace(&wrapped, &clean_rep);
                        } else {
                            return Err(WikiError::InvalidToolArguments {
                                tool: tool_name.to_string(),
                                message: format!(
                                    "`target_text` not found in '{}'",
                                    existing.info.rel_path
                                ),
                            });
                        }
                    }
                }
            }

            if let Some(append_text) = args.get("append_text").and_then(|v| v.as_str()) {
                if !append_text.is_empty() && !updated.contains(append_text.trim()) {
                    if !updated.ends_with('\n') {
                        updated.push('\n');
                    }
                    updated.push_str(append_text);
                    if !updated.ends_with('\n') {
                        updated.push('\n');
                    }
                }
            }

            let info = engine.write_file(scope, &existing.info.scoped_path, &updated)?;
            if scope == CollectionScope::Wiki {
                let _ = engine.rebuild_or_sync_index_md();
            }
            Ok(serde_json::to_value(info)?)
        }

        "wiki_ingest_source" => {
            let req: IngestSourceRequest =
                serde_json::from_value(args).map_err(|e| WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: format!("Invalid ingest arguments: {e}"),
                })?;
            let resp = engine.ingest_source_to_wiki(req)?;
            Ok(serde_json::to_value(resp)?)
        }

        "wiki_graph_neighbors" => {
            if let Some(rel_path) = args.get("rel_path").and_then(|v| v.as_str()) {
                let backlinks = engine.get_backlinks(rel_path)?;
                let outgoing = engine.get_outgoing_links(rel_path)?;
                Ok(json!({
                    "target": rel_path,
                    "backlinks_count": backlinks.len(),
                    "outgoing_count": outgoing.len(),
                    "backlinks": backlinks,
                    "outgoing_links": outgoing,
                }))
            } else {
                let graph = engine.get_graph(None)?;
                Ok(serde_json::to_value(graph)?)
            }
        }

        "wiki_lint" => {
            let report = engine.lint_wiki()?;
            Ok(serde_json::to_value(report)?)
        }

        "wiki_log" => {
            let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("read");
            if mode.eq_ignore_ascii_case("append") {
                let action_str = args
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("update");
                let title = args.get("title").and_then(|v| v.as_str()).ok_or_else(|| {
                    WikiError::InvalidToolArguments {
                        tool: tool_name.to_string(),
                        message: "Missing `title` when appending to log".to_string(),
                    }
                })?;
                let details: Vec<String> = args
                    .get("details")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(ToString::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                engine.append_log(LogAction::from_str_lossy(action_str), title, &details)?;
                Ok(json!({ "status": "appended", "title": title }))
            } else {
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(10);
                let entries = engine.read_recent_logs(limit)?;
                Ok(json!({
                    "count": entries.len(),
                    "entries": entries,
                }))
            }
        }

        "wiki_file_synthesis" => {
            let req: FileSynthesisRequest =
                serde_json::from_value(args).map_err(|e| WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: format!("Invalid synthesis arguments: {e}"),
                })?;
            let info = engine.file_query_synthesis(req)?;
            Ok(serde_json::to_value(info)?)
        }

        "wiki_schema" => {
            let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("read");
            if mode.eq_ignore_ascii_case("write") {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| WikiError::InvalidToolArguments {
                        tool: tool_name.to_string(),
                        message: "Missing `content` when mode='write'".to_string(),
                    })?;
                engine.set_schema_md(content)?;
                Ok(json!({ "status": "updated", "file": "SCHEMA.md" }))
            } else {
                let schema = engine.get_schema_md()?;
                Ok(json!({ "file": "SCHEMA.md", "content": schema }))
            }
        }

        "wiki_delete_file" => {
            let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("wiki");
            let scope = CollectionScope::from_str_opt(scope_str).ok_or_else(|| {
                WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: format!("Invalid scope '{scope_str}', expected 'wiki' or 'sources'"),
                }
            })?;
            let rel_path = args
                .get("rel_path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| WikiError::InvalidToolArguments {
                    tool: tool_name.to_string(),
                    message: "Missing required string parameter `rel_path`".to_string(),
                })?;
            let deleted = engine.delete_file(scope, rel_path)?;
            if scope == CollectionScope::Wiki {
                let _ = engine.rebuild_or_sync_index_md();
            }
            Ok(json!({
                "deleted": deleted,
                "rel_path": rel_path,
            }))
        }

        other => Err(WikiError::UnknownTool(other.to_string())),
    }
}

/// Execute an LLM tool with adaptive token-budgeted reading (`AdaptiveReadBudget`),
/// compact `wiki/index.md` rendering, frontmatter stripping on `sources/...`,
/// automatic embedding/index refresh before `wiki_lint`, and compact response formatting.
pub fn execute_tool_call_with_budget(
    engine: &WikiEngine,
    tool_name: &str,
    args: Value,
    read_budget: AdaptiveReadBudget,
) -> WikiResult<LlmToolOutcome> {
    if tool_name == "wiki_lint" {
        let _ = engine.embed_pending(false);
        let _ = engine.rebuild_or_sync_index_md();
    }

    if tool_name == "wiki_get" {
        let file = args
            .get("file")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| WikiError::InvalidToolArguments {
                tool: tool_name.to_string(),
                message: "Missing required string parameter `file`".to_string(),
            })?;
        let from_line = args
            .get("from_line")
            .or_else(|| args.get("fromLine"))
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let max_lines = args
            .get("max_lines")
            .or_else(|| args.get("maxLines"))
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let line_numbers = args
            .get("line_numbers")
            .or_else(|| args.get("lineNumbers"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let clean_file = file.trim().trim_start_matches('/');
        if (clean_file.eq_ignore_ascii_case("wiki/index.md")
            || clean_file.eq_ignore_ascii_case("index.md"))
            && from_line.is_none()
        {
            let full_idx = engine.read_index_md()?;
            let body = if full_idx.chars().count() <= read_budget.compact_index_max_chars {
                full_idx
            } else {
                engine.read_compact_index_md(read_budget.compact_index_max_chars)?
            };
            return Ok(LlmToolOutcome {
                content: format!("FILE: wiki/index.md\n\n{body}"),
                metadata: json!({ "rel_path": "wiki/index.md" }),
                progress_message: None,
            });
        }

        // For raw source files (`sources/...`), the `# Title` and bullet header already carry
        // the essential metadata, so skip the 30-line YAML frontmatter + `qmd.metadata` block.
        let include_frontmatter = !clean_file.starts_with("sources/");
        let doc = engine.get_document(
            file,
            GetOptions {
                from_line: None,
                max_lines: None,
                line_numbers: false,
                include_frontmatter,
            },
        )?;

        let effective_max_lines = Some(max_lines.unwrap_or(read_budget.default_lines));
        let bounded = slice_lines_with_budget(
            &doc.content,
            from_line,
            effective_max_lines,
            Some(read_budget.max_window_chars),
            line_numbers,
        );

        return Ok(LlmToolOutcome {
            content: format!(
                "FILE: {} (docid: {}, total_lines: {})\n\n{}",
                doc.info.rel_path, doc.info.docid, doc.info.line_count, bounded
            ),
            metadata: json!({
                "rel_path": doc.info.rel_path,
                "docid": doc.info.docid,
                "line_count": doc.info.line_count,
            }),
            progress_message: None,
        });
    }

    let res = execute_tool_call(engine, tool_name, args)?;

    match tool_name {
        "wiki_tree" => {
            let ascii = res.get("ascii_tree").and_then(Value::as_str).unwrap_or("");
            Ok(LlmToolOutcome {
                content: ascii.to_string(),
                metadata: json!({ "total_files": res.get("total_files") }),
                progress_message: None,
            })
        }
        "wiki_lint" => {
            let is_healthy = res
                .get("is_healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let broken_arr = res
                .get("broken_links")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let orphans_arr = res
                .get("orphan_wiki_pages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let uningested_arr = res
                .get("uningested_sources")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let broken = broken_arr.len();
            let orphans = orphans_arr.len();
            let _ = engine.append_log(
                LogAction::Lint,
                "Subagent executed wiki_lint health-check",
                &[format!(
                    "is_healthy={is_healthy}, broken_links={broken}, orphan_wiki_pages={orphans}"
                )],
            );
            let compact = json!({
                "is_healthy": is_healthy,
                "uningested_sources": uningested_arr,
                "broken_links": broken_arr,
                "orphan_wiki_pages": orphans_arr,
            });
            let pretty = serde_json::to_string_pretty(&compact).unwrap_or_default();
            Ok(LlmToolOutcome {
                content: pretty,
                metadata: compact,
                progress_message: Some(format!(
                    "Wiki Lint: is_healthy={is_healthy}, broken_links={broken}, orphans={orphans}"
                )),
            })
        }
        "wiki_ingest_source" => {
            if let Some(p) = res.get("wiki_rel_path").and_then(Value::as_str) {
                let docid = res.get("wiki_docid").and_then(Value::as_str).unwrap_or("");
                let msg = format!("Đã ingest trang Wiki: `{p}`");
                return Ok(LlmToolOutcome {
                    content: format!("Ingested `{p}` (docid: {docid})"),
                    metadata: res,
                    progress_message: Some(msg),
                });
            }
            let pretty = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            Ok(LlmToolOutcome {
                content: pretty,
                metadata: res,
                progress_message: None,
            })
        }
        "wiki_write_file" | "wiki_patch_file" => {
            if let Some(p) = res.get("rel_path").and_then(Value::as_str) {
                let docid = res.get("docid").and_then(Value::as_str).unwrap_or("");
                let msg = format!("Đã cập nhật file Wiki: `{p}`");
                return Ok(LlmToolOutcome {
                    content: format!("Updated `{p}` (docid: {docid})"),
                    metadata: res,
                    progress_message: Some(msg),
                });
            }
            let pretty = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            Ok(LlmToolOutcome {
                content: pretty,
                metadata: res,
                progress_message: None,
            })
        }
        "wiki_file_synthesis" => {
            if let Some(p) = res.get("rel_path").and_then(Value::as_str) {
                let docid = res.get("docid").and_then(Value::as_str).unwrap_or("");
                let msg = format!("Đã lưu trang Synthesis: `{p}`");
                return Ok(LlmToolOutcome {
                    content: format!("Filed synthesis `{p}` (docid: {docid})"),
                    metadata: res,
                    progress_message: Some(msg),
                });
            }
            let pretty = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            Ok(LlmToolOutcome {
                content: pretty,
                metadata: res,
                progress_message: None,
            })
        }
        "wiki_delete_file" => {
            let deleted = res.get("deleted").and_then(Value::as_bool).unwrap_or(false);
            let rel_path = res.get("rel_path").and_then(Value::as_str).unwrap_or("");
            let msg = format!("Đã xóa file Wiki `{rel_path}` (deleted={deleted})");
            Ok(LlmToolOutcome {
                content: format!("deleted={deleted} path={rel_path}"),
                metadata: res,
                progress_message: Some(msg),
            })
        }
        _ => {
            let pretty = serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string());
            Ok(LlmToolOutcome {
                content: pretty,
                metadata: res,
                progress_message: None,
            })
        }
    }
}

fn parse_collections_arg(val: Option<&Value>) -> Vec<CollectionScope> {
    let Some(Value::Array(arr)) = val else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|x| x.as_str().and_then(CollectionScope::from_str_opt))
        .collect()
}
