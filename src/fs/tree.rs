use crate::config::WikiConfig;
use crate::error::WikiResult;
use crate::fs::path_guard::sanitize_rel_path;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Scope of the directory tree query (Requirement #7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeScope {
    /// Entire knowledge base (`sources/`, `wiki/`, `SCHEMA.md`), excluding hidden system folders (`.wiki`, `.git`, `.obsidian`).
    #[default]
    All,
    /// Only the `sources/` folder tree.
    Sources,
    /// Only the `wiki/` folder tree.
    Wiki,
}

/// Node type in the directory tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    Directory,
    File,
}

/// Options for retrieving the directory tree (`get_tree`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeOptions {
    pub scope: TreeScope,
    /// Optional relative subdirectory inside the chosen scope (e.g. `"docs/flow"` or `"concepts"`).
    pub sub_path: Option<String>,
    /// Maximum depth to traverse (`None` = unlimited).
    pub max_depth: Option<usize>,
    /// Attach indexed metadata (`docid`, `title`, `tags`) to markdown file nodes.
    pub include_metadata: bool,
    /// Include non-markdown files (e.g., images in `sources/assets/`).
    pub include_non_markdown: bool,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            scope: TreeScope::All,
            sub_path: None,
            max_depth: None,
            include_metadata: true,
            include_non_markdown: true,
        }
    }
}

impl TreeOptions {
    pub fn for_scope(scope: TreeScope) -> Self {
        Self {
            scope,
            ..Default::default()
        }
    }

    pub fn with_sub_path(mut self, sub_path: impl Into<String>) -> Self {
        self.sub_path = Some(sub_path.into());
        self
    }

    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = Some(max_depth);
        self
    }
}

/// Metadata summary attached to a file node in the directory tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeDocMeta {
    pub docid: String,
    pub title: String,
    pub tags: Vec<String>,
}

/// Recursive directory and file tree node (Requirement #7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTreeNode {
    /// File or directory name (e.g. `"flow"` or `"lib.md"`).
    pub name: String,
    /// Path relative to the requested scope root (e.g. `"docs/flow/lib.md"`).
    pub rel_path: String,
    /// Path relative to `data_root` (e.g. `"sources/docs/flow/lib.md"`).
    pub vault_rel_path: String,
    /// `Directory` or `File`.
    pub node_type: NodeType,
    /// File size in bytes (for `File` nodes).
    pub size_bytes: Option<u64>,
    /// Short QMD document ID (e.g. `"#a1b2c3"`) if indexed.
    pub docid: Option<String>,
    /// Extracted document title if indexed.
    pub title: Option<String>,
    /// Extracted tags if indexed.
    pub tags: Vec<String>,
    /// Sorted children (`Directory` nodes first alphabetically, then `File` nodes alphabetically).
    pub children: Vec<FileTreeNode>,
}

impl FileTreeNode {
    /// Render the directory tree as a clean ASCII tree for human inspection or LLM context.
    pub fn to_ascii_tree(&self) -> String {
        let mut out = String::new();
        let root_label = if self.node_type == NodeType::Directory && self.name != "." {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        };
        out.push_str(&root_label);
        out.push('\n');

        let len = self.children.len();
        for (idx, child) in self.children.iter().enumerate() {
            let is_last = idx + 1 == len;
            child.render_ascii_rec("", is_last, &mut out);
        }
        out
    }

    fn render_ascii_rec(&self, prefix: &str, is_last: bool, out: &mut String) {
        let connector = if is_last { "└── " } else { "├── " };
        out.push_str(prefix);
        out.push_str(connector);

        match self.node_type {
            NodeType::Directory => {
                out.push_str(&self.name);
                out.push_str("/\n");
                let next_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
                let count = self.children.len();
                for (i, child) in self.children.iter().enumerate() {
                    child.render_ascii_rec(&next_prefix, i + 1 == count, out);
                }
            }
            NodeType::File => {
                out.push_str(&self.name);
                if let (Some(docid), Some(title)) = (&self.docid, &self.title) {
                    out.push_str(&format!(" ({docid} - \"{title}\")"));
                } else if let Some(docid) = &self.docid {
                    out.push_str(&format!(" ({docid})"));
                }
                out.push('\n');
            }
        }
    }

    /// Count total files in this tree.
    pub fn total_files(&self) -> usize {
        match self.node_type {
            NodeType::File => 1,
            NodeType::Directory => self.children.iter().map(Self::total_files).sum(),
        }
    }

    /// Count total directories in this tree (excluding the root node itself).
    pub fn total_directories(&self) -> usize {
        match self.node_type {
            NodeType::File => 0,
            NodeType::Directory => self
                .children
                .iter()
                .map(|c| {
                    if c.node_type == NodeType::Directory {
                        1 + c.total_directories()
                    } else {
                        0
                    }
                })
                .sum(),
        }
    }
}

/// Build the recursive `FileTreeNode` from disk and enrich with indexed metadata.
pub fn build_directory_tree(
    config: &WikiConfig,
    opts: &TreeOptions,
    meta_map: &HashMap<String, TreeDocMeta>,
) -> WikiResult<FileTreeNode> {
    let (scope_base, root_name, vault_prefix) = match opts.scope {
        TreeScope::All => (config.data_root.clone(), ".".to_string(), String::new()),
        TreeScope::Sources => (
            config.sources_dir(),
            config.sources_dir_name.clone(),
            config.sources_dir_name.clone(),
        ),
        TreeScope::Wiki => (
            config.wiki_dir(),
            config.wiki_dir_name.clone(),
            config.wiki_dir_name.clone(),
        ),
    };

    let (target_dir, display_name, base_rel_in_scope, base_vault_rel) =
        if let Some(sub) = &opts.sub_path {
            let clean_sub = sanitize_rel_path(sub)?;
            let dir = scope_base.join(&clean_sub);
            let name = Path::new(&clean_sub)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&clean_sub)
                .to_string();
            let v_rel = if vault_prefix.is_empty() {
                clean_sub.clone()
            } else {
                format!("{vault_prefix}/{clean_sub}")
            };
            (dir, name, clean_sub, v_rel)
        } else {
            (scope_base.clone(), root_name, String::new(), vault_prefix)
        };

    if !target_dir.exists() {
        return Ok(FileTreeNode {
            name: display_name,
            rel_path: base_rel_in_scope,
            vault_rel_path: base_vault_rel,
            node_type: NodeType::Directory,
            size_bytes: None,
            docid: None,
            title: None,
            tags: Vec::new(),
            children: Vec::new(),
        });
    }

    build_node_recursive(
        &target_dir,
        &scope_base,
        &config.data_root,
        &display_name,
        0,
        opts,
        meta_map,
    )
}

fn build_node_recursive(
    current_path: &Path,
    scope_base: &Path,
    vault_root: &Path,
    name: &str,
    current_depth: usize,
    opts: &TreeOptions,
    meta_map: &HashMap<String, TreeDocMeta>,
) -> WikiResult<FileTreeNode> {
    let rel_path = current_path
        .strip_prefix(scope_base)
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");
    let vault_rel_path = current_path
        .strip_prefix(vault_root)
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");

    let metadata = std::fs::metadata(current_path)?;
    if metadata.is_file() {
        let doc_meta = if opts.include_metadata {
            meta_map.get(&vault_rel_path)
        } else {
            None
        };
        return Ok(FileTreeNode {
            name: name.to_string(),
            rel_path,
            vault_rel_path,
            node_type: NodeType::File,
            size_bytes: Some(metadata.len()),
            docid: doc_meta.map(|m| m.docid.clone()),
            title: doc_meta.map(|m| m.title.clone()),
            tags: doc_meta.map(|m| m.tags.clone()).unwrap_or_default(),
            children: Vec::new(),
        });
    }

    let mut children = Vec::new();
    let allow_deeper = opts
        .max_depth
        .map(|max| current_depth < max)
        .unwrap_or(true);

    if allow_deeper {
        let mut entries: Vec<_> = std::fs::read_dir(current_path)?
            .filter_map(|e| e.ok())
            .collect();

        entries.sort_by(|a, b| {
            let a_is_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let b_is_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a
                    .file_name()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .cmp(&b.file_name().to_string_lossy().to_ascii_lowercase()),
            }
        });

        for entry in entries {
            let entry_name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden directories/files (.wiki, .git, .obsidian, .DS_Store)
            if entry_name.starts_with('.') {
                continue;
            }
            let entry_path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_dir && !opts.include_non_markdown {
                let is_md = entry_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("md"))
                    .unwrap_or(false);
                if !is_md {
                    continue;
                }
            }

            let child_node = build_node_recursive(
                &entry_path,
                scope_base,
                vault_root,
                &entry_name,
                current_depth + 1,
                opts,
                meta_map,
            )?;
            children.push(child_node);
        }
    }

    Ok(FileTreeNode {
        name: name.to_string(),
        rel_path,
        vault_rel_path,
        node_type: NodeType::Directory,
        size_bytes: None,
        docid: None,
        title: None,
        tags: Vec::new(),
        children,
    })
}
