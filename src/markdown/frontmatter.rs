use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Typed metadata value adhering to QMD's metadata specification:
/// strings, numbers, booleans, or flat homogeneous non-empty arrays of one of those.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TypedMetaValue {
    String(String),
    Number(f64),
    Bool(bool),
    StringArray(Vec<String>),
    NumberArray(Vec<f64>),
    BoolArray(Vec<bool>),
}

impl TypedMetaValue {
    pub fn value_type(&self) -> &'static str {
        match self {
            Self::String(_) | Self::StringArray(_) => "string",
            Self::Number(_) | Self::NumberArray(_) => "number",
            Self::Bool(_) | Self::BoolArray(_) => "boolean",
        }
    }

    pub fn is_array(&self) -> bool {
        matches!(
            self,
            Self::StringArray(_) | Self::NumberArray(_) | Self::BoolArray(_)
        )
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// Parsed YAML frontmatter and body split.
#[derive(Debug, Clone, Default)]
pub struct ParsedFrontmatter {
    /// Raw YAML string (without `---` delimiters) if present.
    pub raw_yaml: Option<String>,
    /// Number of lines occupied by the frontmatter block (including `---` lines).
    pub frontmatter_lines: usize,
    /// Markdown content after the frontmatter block.
    pub body: String,
    /// Full JSON representation of the YAML frontmatter map.
    pub json_map: Option<serde_json::Map<String, serde_json::Value>>,
    /// Explicit `title` field from frontmatter.
    pub title: Option<String>,
    /// Explicit `category` field from frontmatter.
    pub category: Option<String>,
    /// Explicit `source_file` or `source` field from frontmatter.
    pub source_file: Option<String>,
    /// All source files referenced in frontmatter (`source_file`, `source`, or `sources: [...]` array).
    /// Allows a single existing Wiki page to be updated by multiple raw sources without creating new files.
    pub source_files: Vec<String>,
    /// Tags extracted from frontmatter `tags` field.
    pub tags: Vec<String>,
    /// Aliases extracted from frontmatter `aliases` field.
    pub aliases: Vec<String>,
    /// Validated QMD typed metadata entries (from `qmd.metadata` and top-level scalar/homogeneous array properties).
    pub qmd_metadata: BTreeMap<String, TypedMetaValue>,
}

/// Split a Markdown document into its optional YAML frontmatter and body,
/// extracting Obsidian properties and QMD typed metadata.
pub fn parse_frontmatter(full_content: &str) -> ParsedFrontmatter {
    let normalized = full_content.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") && normalized != "---" {
        return ParsedFrontmatter {
            body: normalized,
            ..Default::default()
        };
    }

    let lines: Vec<&str> = normalized.lines().collect();
    if lines.is_empty() || lines[0].trim() != "---" {
        return ParsedFrontmatter {
            body: normalized,
            ..Default::default()
        };
    }

    let mut closing_line_idx = None;
    for (idx, line) in lines.iter().enumerate().skip(1) {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            closing_line_idx = Some(idx);
            break;
        }
    }

    let Some(end_idx) = closing_line_idx else {
        return ParsedFrontmatter {
            body: normalized,
            ..Default::default()
        };
    };

    let yaml_str = lines[1..end_idx].join("\n");
    let body_str = if end_idx + 1 < lines.len() {
        lines[(end_idx + 1)..].join("\n")
    } else {
        String::new()
    };

    let mut parsed = ParsedFrontmatter {
        raw_yaml: Some(yaml_str.clone()),
        frontmatter_lines: end_idx + 1,
        body: body_str,
        ..Default::default()
    };

    if let Ok(serde_json::Value::Object(map)) = serde_yaml::from_str::<serde_json::Value>(&yaml_str)
    {
        // Extract standard Obsidian fields
        if let Some(serde_json::Value::String(t)) = map.get("title") {
            if !t.trim().is_empty() {
                parsed.title = Some(t.trim().to_string());
            }
        }
        if let Some(serde_json::Value::String(c)) = map.get("category") {
            if !c.trim().is_empty() {
                parsed.category = Some(c.trim().to_string());
            }
        }
        if let Some(serde_json::Value::String(s)) =
            map.get("source_file").or_else(|| map.get("source"))
        {
            if !s.trim().is_empty() {
                let clean = s.trim().to_string();
                parsed.source_file = Some(clean.clone());
                parsed.source_files.push(clean);
            }
        }
        if let Some(sources_val) = map.get("sources").or_else(|| map.get("source_files")) {
            for item in extract_raw_string_list(sources_val) {
                if !parsed.source_files.contains(&item) {
                    if parsed.source_file.is_none() {
                        parsed.source_file = Some(item.clone());
                    }
                    parsed.source_files.push(item);
                }
            }
        }

        if let Some(tags_val) = map.get("tags") {
            parsed.tags = extract_string_list(tags_val);
        }
        if let Some(aliases_val) = map.get("aliases").or_else(|| map.get("alias")) {
            parsed.aliases = extract_string_list(aliases_val);
        }

        // 1. Extract top-level properties as QMD metadata (excluding nested objects like `qmd`)
        for (k, v) in &map {
            if k == "qmd" {
                continue;
            }
            if let Some(typed) = validate_qmd_meta_value(v) {
                parsed.qmd_metadata.insert(k.clone(), typed);
            }
        }

        // 2. Extract explicit `qmd.metadata` namespace (overrides top-level keys if duplicated)
        if let Some(serde_json::Value::Object(qmd_obj)) = map.get("qmd") {
            if let Some(serde_json::Value::Object(meta_obj)) = qmd_obj.get("metadata") {
                for (k, v) in meta_obj {
                    if let Some(typed) = validate_qmd_meta_value(v) {
                        parsed.qmd_metadata.insert(k.clone(), typed);
                    }
                }
            }
        }

        parsed.json_map = Some(map);
    }

    parsed
}

fn extract_raw_string_list(val: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match val {
        serde_json::Value::String(s) => {
            if !s.trim().is_empty() {
                out.push(s.trim().to_string());
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let serde_json::Value::String(s) = item {
                    if !s.trim().is_empty() {
                        out.push(s.trim().to_string());
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn extract_string_list(val: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match val {
        serde_json::Value::String(s) => {
            for part in s.split(|c: char| c == ',' || c.is_whitespace()) {
                let clean = part.trim().trim_start_matches('#');
                if !clean.is_empty() {
                    out.push(clean.to_string());
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let serde_json::Value::String(s) = item {
                    let clean = s.trim().trim_start_matches('#');
                    if !clean.is_empty() {
                        out.push(clean.to_string());
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Validate a JSON value against QMD's strict typed metadata grammar:
/// strings, finite numbers, booleans, or non-empty homogeneous flat arrays of one of those.
pub fn validate_qmd_meta_value(val: &serde_json::Value) -> Option<TypedMetaValue> {
    match val {
        serde_json::Value::String(s) => Some(TypedMetaValue::String(s.clone())),
        serde_json::Value::Number(n) => n.as_f64().map(TypedMetaValue::Number),
        serde_json::Value::Bool(b) => Some(TypedMetaValue::Bool(*b)),
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return None;
            }
            if arr.iter().all(|x| x.is_string()) {
                let strs: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(ToString::to_string))
                    .collect();
                Some(TypedMetaValue::StringArray(strs))
            } else if arr.iter().all(|x| x.is_number()) {
                let nums: Vec<f64> = arr.iter().filter_map(|x| x.as_f64()).collect();
                if nums.len() == arr.len() {
                    Some(TypedMetaValue::NumberArray(nums))
                } else {
                    None
                }
            } else if arr.iter().all(|x| x.is_boolean()) {
                let bools: Vec<bool> = arr.iter().filter_map(|x| x.as_bool()).collect();
                Some(TypedMetaValue::BoolArray(bools))
            } else {
                None
            }
        }
        _ => None,
    }
}
