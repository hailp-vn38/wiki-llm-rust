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

/// Build a compact, slug-oriented Wiki Index inspired by `javis-os` (`learn._read_index`):
/// - Lists existing `wiki/sources/`, `wiki/entities/`, `wiki/concepts/`, and `wiki/syntheses/` slugs
///   with titles and `aliases` (omitting verbose descriptions and timestamps to save context tokens).
/// - Stops cleanly at a line boundary if `max_chars` is reached and appends an explicit overflow notice
///   telling the agent how many pages were omitted and instructing it to call `wiki_query`/`wiki_tree`.
pub fn generate_compact_index_md(
    conn: &Connection,
    config: &WikiConfig,
    max_chars: usize,
) -> WikiResult<String> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, scoped_path, title, category, aliases_json
         FROM documents
         WHERE collection = 'wiki' AND scoped_path NOT IN ('index.md', 'log.md')
         ORDER BY
            CASE category
                WHEN 'entity' THEN 0
                WHEN 'concept' THEN 1
                WHEN 'source_summary' THEN 2
                WHEN 'synthesis' THEN 3
                ELSE 4
            END,
            scoped_path ASC",
    )?;

    struct CompactItem {
        rel_path: String,
        scoped_path: String,
        title: String,
        category: String,
        aliases: Vec<String>,
    }

    let items: Vec<CompactItem> = stmt
        .query_map([], |r| {
            let aliases_json: String = r.get(4)?;
            let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
            Ok(CompactItem {
                rel_path: r.get(0)?,
                scoped_path: r.get(1)?,
                title: r.get(2)?,
                category: r
                    .get::<_, Option<String>>(3)?
                    .unwrap_or_else(|| "wiki_page".to_string()),
                aliases,
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

    let budget = max_chars.max(400);
    let mut out = format!(
        "# Compact Wiki Index (`{}/index.md` — {} trang)\n",
        config.wiki_dir_name,
        items.len()
    );
    let mut current_cat = String::new();
    let mut included = 0usize;

    for item in &items {
        let section_header = match item.category.as_str() {
            "entity" => "## Entities (`wiki/entities/`)\n",
            "concept" => "## Concepts (`wiki/concepts/`)\n",
            "source_summary" => "## Sources (`wiki/sources/`)\n",
            "synthesis" => "## Syntheses (`wiki/syntheses/`)\n",
            _ => "## Other (`wiki/`)\n",
        };

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
        let line = format!("- `{slug}` -> [[{link_target}|{}]]{alias_suffix}\n", item.title);

        let needed = (if current_cat != item.category {
            section_header.len()
        } else {
            0
        }) + line.len();

        if included > 0 && out.len() + needed > budget {
            let remaining = items.len() - included;
            out.push_str(&format!(
                "... (còn {remaining} trang nữa không liệt kê hết — dùng `wiki_query` hoặc `wiki_tree` để kiểm tra trước khi tạo trang mới)\n"
            ));
            break;
        }

        if current_cat != item.category {
            current_cat = item.category.clone();
            out.push_str(section_header);
        }
        out.push_str(&line);
        included += 1;
    }

    Ok(out)
}
