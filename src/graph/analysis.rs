use crate::error::WikiResult;
use crate::types::CollectionScope;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A node in the Obsidian Graph View representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    /// Vault-relative path (e.g. `"wiki/sources/docs/flow/lib.md"`).
    pub rel_path: String,
    /// Node label shown in Obsidian Graph View (`file_stem`, e.g. `"lib"`).
    pub label: String,
    /// Document title.
    pub title: String,
    /// Short QMD document ID (e.g. `"#a1b2c3"`).
    pub docid: String,
    /// Collection (`sources` or `wiki`).
    pub collection: CollectionScope,
    /// Document category (`raw_source`, `source_summary`, `entity`, `concept`, `synthesis`, `system`).
    pub category: Option<String>,
    /// Tags associated with this node.
    pub tags: Vec<String>,
    /// Number of incoming links (backlinks) pointing to this document.
    pub inbound_degree: usize,
    /// Number of outgoing links from this document.
    pub outbound_degree: usize,
    /// True if `inbound_degree == 0` and `outbound_degree == 0` (or no inbound links from wiki).
    pub is_orphan: bool,
}

/// A directed edge in the Obsidian Graph View representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Source document `rel_path`.
    pub source_rel_path: String,
    /// Raw link target written in Markdown (e.g. `"bm25-search"` or `"sources/docs/flow/lib.md"`).
    pub raw_target: String,
    /// Target path spec before `#` and `|`.
    pub target_path_spec: String,
    /// Optional heading anchor (`#Heading`).
    pub target_heading: Option<String>,
    /// Optional display alias (`|Alias`).
    pub display_alias: Option<String>,
    /// Link kind (`wikilink`, `embed`, `markdown`).
    pub link_type: String,
    /// Resolved target document `rel_path` (`None` if dangling/unresolved).
    pub resolved_rel_path: Option<String>,
    /// 1-indexed line number in the source document.
    pub line_number: usize,
}

/// Complete Obsidian Graph View dataset (Nodes, Edges, Dangling Targets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Unresolved target specs (`[[missing-page]]`) that appear in links but don't have a file yet.
    pub unresolved_targets: Vec<String>,
}

/// Build the full `GraphData` compatible with Obsidian Graph View.
pub fn build_graph_data(
    conn: &Connection,
    filter_collection: Option<CollectionScope>,
) -> WikiResult<GraphData> {
    let mut edge_stmt = conn.prepare(
        "SELECT source_rel_path, raw_target, target_path_spec, target_heading,
                display_alias, link_type, resolved_rel_path, line_number
         FROM graph_links
         ORDER BY source_rel_path ASC, line_number ASC",
    )?;

    let all_edges: Vec<GraphEdge> = edge_stmt
        .query_map([], |r| {
            Ok(GraphEdge {
                source_rel_path: r.get(0)?,
                raw_target: r.get(1)?,
                target_path_spec: r.get(2)?,
                target_heading: r.get(3)?,
                display_alias: r.get(4)?,
                link_type: r.get(5)?,
                resolved_rel_path: r.get(6)?,
                line_number: r.get::<_, i64>(7)? as usize,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut inbound_counts: HashMap<String, usize> = HashMap::new();
    let mut outbound_counts: HashMap<String, usize> = HashMap::new();
    let mut unresolved_set: HashSet<String> = HashSet::new();

    for edge in &all_edges {
        *outbound_counts
            .entry(edge.source_rel_path.clone())
            .or_insert(0) += 1;
        if let Some(ref target_rel) = edge.resolved_rel_path {
            if target_rel != &edge.source_rel_path {
                *inbound_counts.entry(target_rel.clone()).or_insert(0) += 1;
            }
        } else {
            unresolved_set.insert(edge.target_path_spec.clone());
        }
    }

    let mut doc_stmt = conn.prepare(
        "SELECT rel_path, file_stem, title, docid, collection, category, tags_json
         FROM documents
         ORDER BY rel_path ASC",
    )?;
    let rows = doc_stmt.query_map([], |r| {
        let rel_path: String = r.get(0)?;
        let label: String = r.get(1)?;
        let title: String = r.get(2)?;
        let docid: String = r.get(3)?;
        let coll_str: String = r.get(4)?;
        let category: Option<String> = r.get(5)?;
        let tags_json: String = r.get(6)?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        let collection = CollectionScope::from_str_opt(&coll_str).unwrap_or(CollectionScope::Wiki);
        Ok((rel_path, label, title, docid, collection, category, tags))
    })?;

    let mut nodes = Vec::new();
    let mut allowed_paths = HashSet::new();

    for row in rows {
        let (rel_path, label, title, docid, collection, category, tags) = row?;
        if let Some(filter_coll) = filter_collection {
            if collection != filter_coll {
                continue;
            }
        }
        let inbound = inbound_counts.get(&rel_path).copied().unwrap_or(0);
        let outbound = outbound_counts.get(&rel_path).copied().unwrap_or(0);
        let is_system = category.as_deref() == Some("system");
        let is_orphan = !is_system && inbound == 0;

        allowed_paths.insert(rel_path.clone());
        nodes.push(GraphNode {
            rel_path,
            label,
            title,
            docid,
            collection,
            category,
            tags,
            inbound_degree: inbound,
            outbound_degree: outbound,
            is_orphan,
        });
    }

    let filtered_edges: Vec<GraphEdge> = if filter_collection.is_some() {
        all_edges
            .into_iter()
            .filter(|e| allowed_paths.contains(&e.source_rel_path))
            .collect()
    } else {
        all_edges
    };

    let mut unresolved_targets: Vec<String> = unresolved_set.into_iter().collect();
    unresolved_targets.sort();

    Ok(GraphData {
        nodes,
        edges: filtered_edges,
        unresolved_targets,
    })
}

/// Get all incoming links (backlinks) pointing to a target document.
pub fn get_backlinks_for_doc(conn: &Connection, rel_path: &str) -> WikiResult<Vec<GraphEdge>> {
    let norm = rel_path.trim().replace('\\', "/");
    let resolved_rel = find_canonical_rel_path(conn, &norm)?.unwrap_or(norm);

    let mut stmt = conn.prepare(
        "SELECT source_rel_path, raw_target, target_path_spec, target_heading,
                display_alias, link_type, resolved_rel_path, line_number
         FROM graph_links
         WHERE resolved_rel_path = ?1
         ORDER BY source_rel_path ASC, line_number ASC",
    )?;

    let edges = stmt
        .query_map(params![resolved_rel], |r| {
            Ok(GraphEdge {
                source_rel_path: r.get(0)?,
                raw_target: r.get(1)?,
                target_path_spec: r.get(2)?,
                target_heading: r.get(3)?,
                display_alias: r.get(4)?,
                link_type: r.get(5)?,
                resolved_rel_path: r.get(6)?,
                line_number: r.get::<_, i64>(7)? as usize,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(edges)
}

/// Get all outgoing links starting from a document.
pub fn get_outgoing_links_for_doc(conn: &Connection, rel_path: &str) -> WikiResult<Vec<GraphEdge>> {
    let norm = rel_path.trim().replace('\\', "/");
    let canonical = find_canonical_rel_path(conn, &norm)?.unwrap_or(norm);

    let mut stmt = conn.prepare(
        "SELECT source_rel_path, raw_target, target_path_spec, target_heading,
                display_alias, link_type, resolved_rel_path, line_number
         FROM graph_links
         WHERE source_rel_path = ?1
         ORDER BY line_number ASC",
    )?;

    let edges = stmt
        .query_map(params![canonical], |r| {
            Ok(GraphEdge {
                source_rel_path: r.get(0)?,
                raw_target: r.get(1)?,
                target_path_spec: r.get(2)?,
                target_heading: r.get(3)?,
                display_alias: r.get(4)?,
                link_type: r.get(5)?,
                resolved_rel_path: r.get(6)?,
                line_number: r.get::<_, i64>(7)? as usize,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(edges)
}

fn find_canonical_rel_path(conn: &Connection, input: &str) -> WikiResult<Option<String>> {
    let with_md = if input.ends_with(".md") {
        input.to_string()
    } else {
        format!("{input}.md")
    };
    conn.query_row(
        "SELECT rel_path FROM documents
         WHERE rel_path = ?1 OR rel_path = ?2 OR scoped_path = ?1 OR scoped_path = ?2 OR lower(file_stem) = lower(?1)
         ORDER BY
            CASE WHEN rel_path = ?1 OR rel_path = ?2 THEN 0 ELSE 1 END,
            CASE WHEN collection = 'wiki' THEN 0 ELSE 1 END,
            length(rel_path) ASC
         LIMIT 1",
        params![input, with_md],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Initialize `.obsidian/graph.json` so Obsidian's Graph View automatically color-codes
/// `sources/`, `wiki/sources/`, `wiki/entities/`, `wiki/concepts/`, and `wiki/syntheses/`.
pub fn init_obsidian_graph_config(obsidian_dir: &Path) -> WikiResult<()> {
    std::fs::create_dir_all(obsidian_dir)?;
    let graph_json_path = obsidian_dir.join("graph.json");
    if graph_json_path.exists() {
        return Ok(());
    }

    let config_json = serde_json::json!({
        "collapse-filter": false,
        "search": "",
        "showTags": true,
        "showAttachments": false,
        "hideUnresolved": false,
        "showOrphans": true,
        "collapse-color-groups": false,
        "colorGroups": [
            {
                "query": "path:sources",
                "color": { "a": 1, "rgb": 3447003 }
            },
            {
                "query": "path:wiki/sources",
                "color": { "a": 1, "rgb": 3066993 }
            },
            {
                "query": "path:wiki/entities",
                "color": { "a": 1, "rgb": 15105570 }
            },
            {
                "query": "path:wiki/concepts",
                "color": { "a": 1, "rgb": 10181046 }
            },
            {
                "query": "path:wiki/syntheses",
                "color": { "a": 1, "rgb": 15277667 }
            }
        ],
        "collapse-display": false,
        "showArrow": true,
        "textFadeMultiplier": 0,
        "nodeSizeMultiplier": 1.1,
        "lineSizeMultiplier": 1,
        "collapse-forces": false,
        "centerStrength": 0.5,
        "repelStrength": 10,
        "linkStrength": 1,
        "linkDistance": 250
    });

    std::fs::write(graph_json_path, serde_json::to_string_pretty(&config_json)?)?;
    Ok(())
}
