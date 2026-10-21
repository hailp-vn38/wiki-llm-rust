use crate::config::WikiConfig;
use crate::error::WikiResult;
use rusqlite::Connection;

/// Default `SCHEMA.md` content created in `<data_root>/SCHEMA.md` when initializing a vault.
pub const DEFAULT_SCHEMA_MD: &str = r#"# LLM Wiki Schema & Operating Conventions

You are the disciplined maintainer of this persistent **LLM Wiki** knowledge base (compatible with Obsidian Graph View and powered by QMD hybrid search).

## 1. Three-Layer Architecture
1. **`sources/` (Raw Sources)**: Curated collection of source documents. Immutable — read from them, never modify raw sources during synthesis.
2. **`wiki/` (The Wiki)**: LLM-owned directory of interlinked Markdown pages:
   - `wiki/sources/`: 1-to-1 summaries of files in `sources/`. **CRITICAL RULE**: The wiki filename MUST be identical to the source filename (e.g., `sources/docs/flow/lib.md` -> `wiki/sources/docs/flow/lib.md`). Never use auto-generated IDs or hashes as filenames.
   - `wiki/entities/`: Pages for concrete entities (projects, libraries, people, organizations).
   - `wiki/concepts/`: Pages for domain concepts, algorithms, architectures, and patterns.
   - `wiki/syntheses/`: Deep-dive comparisons, theses, and filed query analyses.
   - `wiki/index.md`: Content-oriented master catalog of every page in the wiki.
   - `wiki/log.md`: Chronological append-only record of ingests, queries, and lint passes.
3. **`SCHEMA.md` (The Schema)**: This configuration document defining conventions and workflows.

## 2. Obsidian Graph View Conventions
- Connect pages richly using Obsidian WikiLinks: `[[file-stem]]` or `[[relative/path|Display Alias]]`.
- Every wiki page MUST include YAML frontmatter with `title`, `category`, `tags`, and `updated_at`.
- Source summary pages MUST include `source_file: "[[sources/<path>|<filename>]]"` so Obsidian Graph View links the wiki summary directly to the raw source.

## 3. Core Operations
- **Ingest**: Read raw source -> Call `wiki_ingest_source` -> Create/update `entities/` and `concepts/` pages with cross-links -> Update `wiki/index.md` and `wiki/log.md`.
- **Query**: Search via `wiki_query` and read `wiki/index.md` -> Synthesize answer with `[[WikiLinks]]` citations -> File valuable syntheses back into `wiki/syntheses/`.
- **Lint**: Periodically run `wiki_lint` -> Resolve broken `[[WikiLinks]]`, connect orphan pages, and ingest un-ingested sources.
"#;

/// Generate or rebuild `wiki/index.md` from all currently indexed wiki pages in SQLite,
/// preserving custom one-line summaries stored in frontmatter `summary` when available.
pub fn generate_index_md_content(
    conn: &Connection,
    config: &WikiConfig,
    summary_overrides: &std::collections::HashMap<String, String>,
) -> WikiResult<String> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, scoped_path, file_name, file_stem, title, category, source_rel_path, frontmatter_json, raw_body, indexed_at
         FROM documents
         WHERE collection = 'wiki' AND scoped_path NOT IN ('index.md', 'log.md')
         ORDER BY scoped_path ASC",
    )?;

    struct IndexEntry {
        rel_path: String,
        scoped_path: String,
        file_name: String,
        file_stem: String,
        title: String,
        category: String,
        source_rel_path: Option<String>,
        source_count: usize,
        updated_date: String,
        summary: Option<String>,
    }

    let rows = stmt.query_map([], |r| {
        let rel_path: String = r.get(0)?;
        let scoped_path: String = r.get(1)?;
        let file_name: String = r.get(2)?;
        let file_stem: String = r.get(3)?;
        let title: String = r.get(4)?;
        let category: Option<String> = r.get(5)?;
        let source_rel_path: Option<String> = r.get(6)?;
        let fm_json: Option<String> = r.get(7)?;
        let raw_body: String = r.get(8)?;
        let indexed_at: String = r.get(9)?;

        let mut summary = None;
        let mut source_count = usize::from(source_rel_path.is_some());
        let mut updated_date = indexed_at.chars().take(10).collect::<String>();

        if let Some(js) = fm_json {
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&js) {
                if let Some(serde_json::Value::String(s)) = map.get("summary") {
                    if !s.trim().is_empty() {
                        summary = Some(s.trim().to_string());
                    }
                }
                if let Some(serde_json::Value::Number(n)) = map.get("source_count") {
                    if let Some(c) = n.as_u64() {
                        source_count = c as usize;
                    }
                } else if let Some(serde_json::Value::Array(arr)) = map.get("sources") {
                    source_count = arr.len();
                }
                if let Some(serde_json::Value::String(u)) = map.get("updated_at") {
                    if u.len() >= 10 {
                        updated_date = u[..10].to_string();
                    }
                }
            }
        }

        // Fallback one-line summary from first non-heading, non-quote prose line in body
        if summary.is_none() {
            for line in raw_body.lines() {
                let t = line.trim();
                if !t.is_empty()
                    && !t.starts_with('#')
                    && !t.starts_with('>')
                    && !t.starts_with("```")
                    && !t.starts_with("---")
                {
                    let truncated: String = t.chars().take(120).collect();
                    summary = Some(truncated);
                    break;
                }
            }
        }

        Ok(IndexEntry {
            rel_path,
            scoped_path,
            file_name,
            file_stem,
            title,
            category: category.unwrap_or_else(|| "wiki_page".to_string()),
            source_rel_path,
            source_count,
            updated_date,
            summary,
        })
    })?;

    let mut source_summaries = Vec::new();
    let mut entities = Vec::new();
    let mut concepts = Vec::new();
    let mut syntheses = Vec::new();
    let mut others = Vec::new();

    for row in rows {
        let mut entry = row?;
        if let Some(override_sum) = summary_overrides
            .get(&entry.rel_path)
            .or_else(|| summary_overrides.get(&entry.scoped_path))
        {
            entry.summary = Some(override_sum.clone());
        }

        match entry.category.as_str() {
            "source_summary" => source_summaries.push(entry),
            "entity" => entities.push(entry),
            "concept" => concepts.push(entry),
            "synthesis" => syntheses.push(entry),
            _ => others.push(entry),
        }
    }

    let mut md = String::from(
        "---\ntitle: \"Wiki Master Index\"\ncategory: \"system\"\ntags:\n  - \"wiki/system\"\n  - \"wiki/index\"\n---\n\n# Wiki Knowledge Base Index\n\n> Content-oriented catalog of everything in the wiki.\n\n",
    );

    let format_entry = |e: &IndexEntry| -> String {
        let link_target = e.rel_path.strip_suffix(".md").unwrap_or(&e.rel_path);
        let desc = e.summary.as_deref().unwrap_or(&e.title);
        if let Some(ref src) = e.source_rel_path {
            format!(
                "- [[{link_target}|{}]] — {desc} *(sources: {}, updated: {}, primary: [[{src}|{}]])*\n",
                e.file_name, e.source_count.max(1), e.updated_date, e.file_name
            )
        } else {
            format!(
                "- [[{link_target}|{}]] — {desc} *(updated: {})*\n",
                e.title, e.updated_date
            )
        }
    };

    md.push_str(&format!(
        "## Source Summaries (`{}/sources/`)\n",
        config.wiki_dir_name
    ));
    if source_summaries.is_empty() {
        md.push_str("_No source summaries ingested yet._\n\n");
    } else {
        for e in &source_summaries {
            md.push_str(&format_entry(e));
        }
        md.push('\n');
    }

    md.push_str(&format!(
        "## Entities (`{}/entities/`)\n",
        config.wiki_dir_name
    ));
    if entities.is_empty() {
        md.push_str("_No entity pages yet._\n\n");
    } else {
        for e in &entities {
            md.push_str(&format_entry(e));
        }
        md.push('\n');
    }

    md.push_str(&format!(
        "## Concepts (`{}/concepts/`)\n",
        config.wiki_dir_name
    ));
    if concepts.is_empty() {
        md.push_str("_No concept pages yet._\n\n");
    } else {
        for e in &concepts {
            md.push_str(&format_entry(e));
        }
        md.push('\n');
    }

    md.push_str(&format!(
        "## Syntheses & Analyses (`{}/syntheses/`)\n",
        config.wiki_dir_name
    ));
    if syntheses.is_empty() {
        md.push_str("_No synthesis pages yet._\n\n");
    } else {
        for e in &syntheses {
            md.push_str(&format_entry(e));
        }
        md.push('\n');
    }

    if !others.is_empty() {
        md.push_str("## Other Wiki Pages\n");
        for e in &others {
            md.push_str(&format_entry(e));
        }
        md.push('\n');
    }

    let _ = source_summaries.first().map(|e| &e.file_stem);
    Ok(md)
}

/// Default character budget for compact index injection (mirrors `javis-os` `WIKI_INDEX_MAX = 2500`).
pub const DEFAULT_COMPACT_INDEX_MAX_CHARS: usize = 3_000;

/// Extract `<domain>/<topic>` (or `<domain>`) for a document from its frontmatter JSON or scoped path.
pub fn extract_topic_from_doc(
    scoped_path: &str,
    category: &str,
    fm_json: Option<&str>,
) -> Option<String> {
    if let Some(js) = fm_json {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(js) {
            let domain = map
                .get("domain")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let topic = map
                .get("topic")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match (domain, topic) {
                (Some(d), Some(t)) if d != t => return Some(format!("{d}/{t}")),
                (Some(d), _) => return Some(d.to_string()),
                (None, Some(t)) => return Some(t.to_string()),
                _ => {}
            }

            if let Some(serde_json::Value::Array(tags)) = map.get("tags") {
                for tag in tags.iter().filter_map(|v| v.as_str()) {
                    let clean = tag.trim().trim_start_matches('#');
                    if let Some(rest) = clean.strip_prefix("topic/") {
                        if !rest.trim().is_empty() {
                            return Some(rest.trim().to_string());
                        }
                    }
                }
            }
        }
    }

    let clean = scoped_path.trim_start_matches('/');
    if category == "source_summary" || clean.starts_with("sources/") {
        let under_sources = clean.strip_prefix("sources/").unwrap_or(clean);
        if let Some((parent, _)) = under_sources.rsplit_once('/') {
            if !parent.trim().is_empty() {
                return Some(parent.trim().to_string());
            }
        }
    } else {
        for prefix in ["entities/", "concepts/", "syntheses/"] {
            if let Some(rest) = clean.strip_prefix(prefix) {
                if let Some((parent, _)) = rest.rsplit_once('/') {
                    if !parent.trim().is_empty() {
                        return Some(parent.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

/// Infer the most recently active `<domain>/<topic>` in the Vault from the latest indexed source or wiki page.
pub fn infer_latest_active_topic(conn: &Connection) -> Option<String> {
    let mut stmt = conn
        .prepare(
            "SELECT scoped_path, COALESCE(category, ''), frontmatter_json
             FROM documents
             WHERE scoped_path NOT IN ('index.md', 'log.md')
             ORDER BY
                CASE WHEN collection = 'sources' OR category = 'source_summary' THEN 0 ELSE 1 END,
                id DESC
             LIMIT 15",
        )
        .ok()?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .ok()?;

    for row in rows.flatten() {
        if let Some(t) = extract_topic_from_doc(&row.0, &row.1, row.2.as_deref()) {
            return Some(t);
        }
    }
    None
}

/// Find all `<domain>/<topic>` folders under `wiki/sources/` that do not yet have a corresponding
/// synthesis page in `wiki/syntheses/`.
pub fn find_unsynthesized_topics(conn: &Connection) -> WikiResult<Vec<String>> {
    let mut src_stmt = conn.prepare(
        "SELECT id, scoped_path, file_stem, frontmatter_json
         FROM documents
         WHERE collection = 'wiki' AND (category = 'source_summary' OR scoped_path LIKE 'sources/%')",
    )?;
    let src_rows = src_stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;

    // Map topic_folder -> list of (doc_id, scoped_path, file_stem)
    let mut topic_sources: std::collections::BTreeMap<String, Vec<(i64, String, String)>> =
        std::collections::BTreeMap::new();
    for row in src_rows {
        let (doc_id, scoped_path, file_stem, fm_json) = row?;
        if let Some(topic) =
            extract_topic_from_doc(&scoped_path, "source_summary", fm_json.as_deref())
        {
            topic_sources
                .entry(topic)
                .or_default()
                .push((doc_id, scoped_path, file_stem));
        }
    }

    if topic_sources.is_empty() {
        return Ok(Vec::new());
    }

    let mut synth_stmt = conn.prepare(
        "SELECT id, scoped_path, title, raw_body, COALESCE(frontmatter_json, '')
         FROM documents
         WHERE collection = 'wiki' AND (category = 'synthesis' OR scoped_path LIKE 'syntheses/%')",
    )?;
    struct SynthDoc {
        id: i64,
        scoped_path: String,
        title: String,
        raw_body: String,
        fm_json: String,
    }
    let syntheses: Vec<SynthDoc> = synth_stmt
        .query_map([], |r| {
            Ok(SynthDoc {
                id: r.get(0)?,
                scoped_path: r.get(1)?,
                title: r.get(2)?,
                raw_body: r.get(3)?,
                fm_json: r.get(4)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    if syntheses.is_empty() {
        return Ok(topic_sources.into_keys().collect());
    }

    // Load graph links between synthesis pages and source summary pages
    let mut linked_doc_pairs: std::collections::HashSet<(i64, i64)> =
        std::collections::HashSet::new();
    if let Ok(mut link_stmt) = conn.prepare(
        "SELECT source_doc_id, resolved_doc_id
         FROM graph_links
         WHERE resolved_doc_id IS NOT NULL",
    ) {
        if let Ok(rows) = link_stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        {
            for pair in rows.flatten() {
                linked_doc_pairs.insert(pair);
            }
        }
    }

    let mut unsynthesized = Vec::new();
    for (topic, sources) in topic_sources {
        let topic_lower = topic.to_ascii_lowercase();
        let topic_leaf = topic_lower
            .rsplit_once('/')
            .map(|(_, leaf)| leaf)
            .unwrap_or(&topic_lower);

        let is_covered = syntheses.iter().any(|s| {
            let synth_topic =
                extract_topic_from_doc(&s.scoped_path, "synthesis", Some(&s.fm_json))
                    .map(|t| t.to_ascii_lowercase());
            if synth_topic.as_deref() == Some(topic_lower.as_str()) {
                return true;
            }
            let haystack = format!(
                "{} {} {} {}",
                s.scoped_path.to_ascii_lowercase(),
                s.title.to_ascii_lowercase(),
                s.fm_json.to_ascii_lowercase(),
                s.raw_body.to_ascii_lowercase()
            );
            if haystack.contains(&topic_lower)
                || (topic_leaf.len() >= 3 && haystack.contains(topic_leaf))
            {
                return true;
            }
            sources.iter().any(|(src_id, src_scoped, src_stem)| {
                linked_doc_pairs.contains(&(s.id, *src_id))
                    || linked_doc_pairs.contains(&(*src_id, s.id))
                    || haystack.contains(&src_scoped.to_ascii_lowercase())
                    || (src_stem.len() >= 4 && haystack.contains(&src_stem.to_ascii_lowercase()))
            })
        });

        if !is_covered {
            unsynthesized.push(topic);
        }
    }

    Ok(unsynthesized)
}

/// Build a compact, slug-oriented Wiki Index inspired by `javis-os` (`learn._read_index`).
/// Delegates to [`generate_compact_index_md_for_topic`] with automatic active-topic prioritization.
pub fn generate_compact_index_md(
    conn: &Connection,
    config: &WikiConfig,
    max_chars: usize,
) -> WikiResult<String> {
    generate_compact_index_md_for_topic(conn, config, max_chars, None)
}

/// Build a topic-aware compact Wiki Index for Multi-Topic Vaults:
/// - Prioritizes pages belonging to `preferred_topic` (or the most recently active `<domain>/<topic>`
///   when `preferred_topic` is `None`) so the active topic's Entities, Concepts, Sources, and Syntheses
///   are never starved by older topics when the Vault grows large.
/// - Fills any remaining `max_chars` budget with pages from other topics (most recent first) to enable
///   cross-domain `[[WikiLink]]` reuse.
pub fn generate_compact_index_md_for_topic(
    conn: &Connection,
    config: &WikiConfig,
    max_chars: usize,
    preferred_topic: Option<&str>,
) -> WikiResult<String> {
    let active_topic = preferred_topic
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_start_matches("sources/").trim_start_matches("topic/").to_string())
        .or_else(|| infer_latest_active_topic(conn));

    let active_lower = active_topic.as_ref().map(|s| s.to_ascii_lowercase());
    let active_domain = active_lower
        .as_deref()
        .map(|t| t.split('/').next().unwrap_or(t));

    let mut stmt = conn.prepare(
        "SELECT id, rel_path, scoped_path, title, category, aliases_json, frontmatter_json
         FROM documents
         WHERE collection = 'wiki' AND scoped_path NOT IN ('index.md', 'log.md')
         ORDER BY id DESC",
    )?;

    struct CompactItem {
        id: i64,
        rel_path: String,
        scoped_path: String,
        title: String,
        category: String,
        cat_order: u8,
        aliases: Vec<String>,
        topic: Option<String>,
        topic_tier: u8,
    }

    let mut items: Vec<CompactItem> = stmt
        .query_map([], |r| {
            let id: i64 = r.get(0)?;
            let rel_path: String = r.get(1)?;
            let scoped_path: String = r.get(2)?;
            let title: String = r.get(3)?;
            let category = r
                .get::<_, Option<String>>(4)?
                .unwrap_or_else(|| "wiki_page".to_string());
            let aliases_json: String = r.get(5)?;
            let fm_json: Option<String> = r.get(6)?;
            let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
            let topic = extract_topic_from_doc(&scoped_path, &category, fm_json.as_deref());
            let topic_lower = topic.as_ref().map(|t| t.to_ascii_lowercase());
            let item_domain = topic_lower
                .as_deref()
                .map(|t| t.split('/').next().unwrap_or(t));

            let is_shared = item_domain == Some("shared")
                || scoped_path.to_ascii_lowercase().starts_with("entities/shared/");
            let topic_tier = match (&active_lower, &topic_lower) {
                (Some(act), Some(it)) if act == it || it.starts_with(&format!("{act}/")) || act.starts_with(&format!("{it}/")) => 0,
                _ if active_domain.is_some() && (active_domain == item_domain || is_shared) => 1,
                _ => 2,
            };

            let cat_order = match category.as_str() {
                "entity" => 0,
                "concept" => 1,
                "source_summary" => 2,
                "synthesis" => 3,
                _ => 4,
            };

            Ok(CompactItem {
                id,
                rel_path,
                scoped_path,
                title,
                category,
                cat_order,
                aliases,
                topic,
                topic_tier,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    if items.is_empty() {
        return Ok(format!(
            "# Compact Wiki Index (`{}/index.md`)\n_Chưa có trang Wiki nào._\n",
            config.wiki_dir_name
        ));
    }

    // 1. Select items within budget by prioritizing `topic_tier` (0 -> 1 -> 2), then category order, then recency (`id DESC`)
    items.sort_by(|a, b| {
        a.topic_tier
            .cmp(&b.topic_tier)
            .then_with(|| a.cat_order.cmp(&b.cat_order))
            .then_with(|| b.id.cmp(&a.id))
            .then_with(|| a.scoped_path.cmp(&b.scoped_path))
    });

    let budget = max_chars.max(400);
    let header = if let Some(ref act) = active_topic {
        format!(
            "# Compact Wiki Index (`{}/index.md` — {} trang, ưu tiên chủ đề: `{act}`)\n",
            config.wiki_dir_name,
            items.len()
        )
    } else {
        format!(
            "# Compact Wiki Index (`{}/index.md` — {} trang)\n",
            config.wiki_dir_name,
            items.len()
        )
    };

    let format_item_line = |item: &CompactItem| -> String {
        let link_target = item
            .rel_path
            .strip_suffix(".md")
            .unwrap_or(&item.rel_path);
        let slug = item
            .scoped_path
            .strip_suffix(".md")
            .unwrap_or(&item.scoped_path);
        let alias_suffix = if item.aliases.is_empty() {
            String::new()
        } else {
            format!(" [aliases: {}]", item.aliases.join(", "))
        };
        let topic_suffix = match (&item.topic, item.category.as_str()) {
            (Some(t), "entity" | "concept" | "synthesis") => format!(" (topic: {t})"),
            _ => String::new(),
        };
        format!(
            "- `{slug}` -> [[{link_target}|{}]]{alias_suffix}{topic_suffix}\n",
            item.title
        )
    };

    let section_header_for = |cat: &str| -> &'static str {
        match cat {
            "entity" => "## Entities (`wiki/entities/`)\n",
            "concept" => "## Concepts (`wiki/concepts/`)\n",
            "source_summary" => "## Sources (`wiki/sources/`)\n",
            "synthesis" => "## Syntheses (`wiki/syntheses/`)\n",
            _ => "## Other (`wiki/`)\n",
        }
    };

    // Reserve space for up to 4 section headers so reordering by category after selection stays within budget
    let mut used_chars = header.len() + 140;
    let mut selected_indices = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let line = format_item_line(item);
        if !selected_indices.is_empty() && used_chars + line.len() > budget {
            break;
        }
        used_chars += line.len();
        selected_indices.push(idx);
    }

    let total_items = items.len();
    let included = selected_indices.len();

    // Sort selected items by `(cat_order, topic_tier, scoped_path)` so output is cleanly grouped by category
    selected_indices.sort_by(|&ia, &ib| {
        let a = &items[ia];
        let b = &items[ib];
        a.cat_order
            .cmp(&b.cat_order)
            .then_with(|| a.topic_tier.cmp(&b.topic_tier))
            .then_with(|| a.scoped_path.cmp(&b.scoped_path))
    });

    let mut out = header;
    let mut current_cat = String::new();
    for idx in selected_indices {
        let item = &items[idx];
        if current_cat != item.category {
            current_cat = item.category.clone();
            out.push_str(section_header_for(&item.category));
        }
        out.push_str(&format_item_line(item));
    }

    if included < total_items {
        let remaining = total_items - included;
        out.push_str(&format!(
            "... (còn {remaining} trang thuộc các chủ đề khác không liệt kê hết — dùng `wiki_query` hoặc `wiki_tree` để kiểm tra trước khi tạo trang mới)\n"
        ));
    }

    Ok(out)
}
