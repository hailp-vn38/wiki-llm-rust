use crate::db::filter_sql::MetadataFilter;
use crate::error::{WikiError, WikiResult};
use crate::fs::tree::{TreeOptions, TreeScope};
use crate::search::metadata::MetadataDiscoveryRequest;
use crate::search::{HybridQueryRequest, SubQuery};
use crate::types::{CollectionScope, GetOptions, MultiGetOptions};
use crate::wiki::ingest::{FileSynthesisRequest, IngestSourceRequest};
use crate::wiki::log_md::LogAction;
use crate::WikiEngine;
use serde_json::{json, Value};

/// Execute one of the 12 LLM tools by name with JSON arguments and return a structured JSON response.
pub fn execute_tool_call(engine: &WikiEngine, tool_name: &str, args: Value) -> WikiResult<Value> {
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

        other => Err(WikiError::UnknownTool(other.to_string())),
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
