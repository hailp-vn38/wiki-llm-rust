use crate::db::filter_sql::{eval_filter_on_doc, resolve_matching_document_ids, MetadataFilter};
use crate::error::WikiResult;
use crate::markdown::frontmatter::TypedMetaValue;
use crate::types::CollectionScope;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Sort order for QMD Metadata Discovery values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetadataSortOrder {
    #[default]
    Count,
    Value,
}

/// Request parameters for QMD Metadata Discovery (`discover_metadata`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetadataDiscoveryRequest {
    /// Restrict discovery to specific collections (empty = both `wiki` and `sources`).
    #[serde(default)]
    pub collections: Vec<CollectionScope>,
    /// Optional predicate evaluated against each metadata entry (`field` = `"key"` or `"value"`).
    pub match_cond: Option<MetadataFilter>,
    /// Optional document filter narrowing which documents are counted.
    pub filter: Option<MetadataFilter>,
    /// Max keys reported (default: 50).
    pub key_limit: Option<usize>,
    /// Keys skipped before window (default: 0).
    pub key_offset: Option<usize>,
    /// Max values reported per key and type (default: 10).
    pub value_limit: Option<usize>,
    /// Values skipped per key and type (default: 0).
    pub value_offset: Option<usize>,
    /// Sort values by `count` (default) or `value`.
    #[serde(default)]
    pub sort: MetadataSortOrder,
    /// Hide values held by fewer documents than `min_count` (default: 1).
    pub min_count: Option<usize>,
}

/// Result returned by QMD Metadata Discovery (`discover_metadata`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataDiscoveryResult {
    /// Total active documents in scope.
    pub documents: usize,
    /// Number of documents passing `filter` (if a filter was provided).
    pub filtered_documents: Option<usize>,
    /// Total matching keys before windowing.
    pub total_keys: usize,
    /// Remaining keys after `key_offset + key_limit`.
    pub remaining_keys: usize,
    /// Discovered metadata keys in coverage order.
    pub keys: Vec<DiscoveredMetadataKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredMetadataKey {
    pub key: String,
    pub documents: usize,
    pub types: Vec<DiscoveredMetadataTypeBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredMetadataTypeBucket {
    pub value_type: String,
    pub multi_valued: bool,
    pub documents: usize,
    pub distinct_values: usize,
    pub values: Vec<DiscoveredValueCount>,
    pub remaining_values: usize,
    pub range: Option<NumericRangeStats>,
    pub collections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredValueCount {
    pub value: serde_json::Value,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumericRangeStats {
    pub min: f64,
    pub median: f64,
    pub max: f64,
}

/// Run QMD Metadata Discovery across indexed documents.
pub fn discover_metadata(
    conn: &Connection,
    req: &MetadataDiscoveryRequest,
) -> WikiResult<MetadataDiscoveryResult> {
    let allowed_collections: HashSet<&str> = if req.collections.is_empty() {
        [
            CollectionScope::Wiki.as_str(),
            CollectionScope::Sources.as_str(),
        ]
        .into_iter()
        .collect()
    } else {
        req.collections
            .iter()
            .map(CollectionScope::as_str)
            .collect()
    };

    let filtered_ids: Option<HashSet<i64>> = match &req.filter {
        Some(f) => Some(resolve_matching_document_ids(conn, f)?),
        None => None,
    };

    let mut doc_stmt = conn.prepare("SELECT id, collection FROM documents")?;
    let all_docs: Vec<(i64, String)> = doc_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut active_doc_collections: HashMap<i64, String> = HashMap::new();
    let mut total_in_scope = 0usize;

    for (doc_id, coll) in all_docs {
        if !allowed_collections.contains(coll.as_str()) {
            continue;
        }
        total_in_scope += 1;
        if let Some(ref f_ids) = filtered_ids {
            if !f_ids.contains(&doc_id) {
                continue;
            }
        }
        active_doc_collections.insert(doc_id, coll);
    }

    let filtered_documents = filtered_ids.map(|_| active_doc_collections.len());

    // Query all `document_metadata` rows
    let mut meta_stmt = conn.prepare(
        "SELECT document_id, meta_key, value_type, is_array, val_str, val_num, val_bool
         FROM document_metadata",
    )?;
    let rows = meta_stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)? != 0,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<f64>>(5)?,
            r.get::<_, Option<i64>>(6)?,
        ))
    })?;

    // Aggregate: key -> (set of doc_ids, type -> TypeAccumulator)
    struct TypeAccum {
        multi_valued: bool,
        docs: HashSet<i64>,
        collections: HashSet<String>,
        value_docs: HashMap<String, (serde_json::Value, HashSet<i64>)>,
        numeric_values: Vec<f64>,
    }

    let mut key_map: HashMap<String, (HashSet<i64>, BTreeMap<String, TypeAccum>)> = HashMap::new();

    for row in rows {
        let (doc_id, meta_key, value_type, is_array, val_str, val_num, val_bool) = row?;
        let Some(coll) = active_doc_collections.get(&doc_id) else {
            continue;
        };

        let (json_val, typed_val, canonical_val_key) = match value_type.as_str() {
            "string" => {
                let s = val_str.unwrap_or_default();
                (
                    serde_json::Value::String(s.clone()),
                    TypedMetaValue::String(s.clone()),
                    format!("s:{s}"),
                )
            }
            "number" => {
                let n = val_num.unwrap_or(0.0);
                (
                    serde_json::json!(n),
                    TypedMetaValue::Number(n),
                    format!("n:{n:.6}"),
                )
            }
            "boolean" => {
                let b = val_bool.unwrap_or(0) != 0;
                (
                    serde_json::Value::Bool(b),
                    TypedMetaValue::Bool(b),
                    format!("b:{b}"),
                )
            }
            _ => continue,
        };

        // Evaluate `match_cond` against entry `{"key": meta_key, "value": typed_val}`
        if let Some(ref match_filter) = req.match_cond {
            let mut entry_map = BTreeMap::new();
            entry_map.insert("key".to_string(), TypedMetaValue::String(meta_key.clone()));
            entry_map.insert("value".to_string(), typed_val);
            if !eval_filter_on_doc(match_filter, &entry_map)? {
                continue;
            }
        }

        let (key_docs, type_buckets) = key_map
            .entry(meta_key)
            .or_insert_with(|| (HashSet::new(), BTreeMap::new()));
        key_docs.insert(doc_id);

        let bucket = type_buckets
            .entry(value_type.clone())
            .or_insert_with(|| TypeAccum {
                multi_valued: false,
                docs: HashSet::new(),
                collections: HashSet::new(),
                value_docs: HashMap::new(),
                numeric_values: Vec::new(),
            });

        if is_array {
            bucket.multi_valued = true;
        }
        bucket.docs.insert(doc_id);
        bucket.collections.insert(coll.clone());
        let val_entry = bucket
            .value_docs
            .entry(canonical_val_key)
            .or_insert_with(|| (json_val, HashSet::new()));
        val_entry.1.insert(doc_id);

        if value_type == "number" {
            if let Some(n) = val_num {
                bucket.numeric_values.push(n);
            }
        }
    }

    let min_count = req.min_count.unwrap_or(1).max(1);
    let value_limit = req.value_limit.unwrap_or(10);
    let value_offset = req.value_offset.unwrap_or(0);
    let key_limit = req.key_limit.unwrap_or(50);
    let key_offset = req.key_offset.unwrap_or(0);

    let mut discovered_keys: Vec<DiscoveredMetadataKey> = Vec::new();
    for (key, (key_docs, type_buckets)) in key_map {
        let mut bucket_list = Vec::new();
        for (v_type, mut accum) in type_buckets {
            let mut val_counts: Vec<DiscoveredValueCount> = accum
                .value_docs
                .into_values()
                .map(|(v, d_set)| DiscoveredValueCount {
                    value: v,
                    count: d_set.len(),
                })
                .filter(|vc| vc.count >= min_count)
                .collect();

            if val_counts.is_empty() {
                continue;
            }

            let distinct_values = val_counts.len();
            match req.sort {
                MetadataSortOrder::Count => {
                    val_counts.sort_by(|a, b| {
                        b.count
                            .cmp(&a.count)
                            .then_with(|| a.value.to_string().cmp(&b.value.to_string()))
                    });
                }
                MetadataSortOrder::Value => {
                    val_counts.sort_by_key(|a| a.value.to_string());
                }
            }

            let windowed_vals: Vec<DiscoveredValueCount> = val_counts
                .into_iter()
                .skip(value_offset)
                .take(value_limit)
                .collect();
            let shown_end = (value_offset + windowed_vals.len()).min(distinct_values);
            let remaining_values = distinct_values.saturating_sub(shown_end);

            let range = if v_type == "number" && !accum.numeric_values.is_empty() {
                accum
                    .numeric_values
                    .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let min = *accum.numeric_values.first().unwrap_or(&0.0);
                let max = *accum.numeric_values.last().unwrap_or(&0.0);
                let mid = accum.numeric_values.len() / 2;
                let median = accum.numeric_values[mid];
                Some(NumericRangeStats { min, median, max })
            } else {
                None
            };

            let mut collections: Vec<String> = accum.collections.into_iter().collect();
            collections.sort();

            bucket_list.push(DiscoveredMetadataTypeBucket {
                value_type: v_type,
                multi_valued: accum.multi_valued,
                documents: accum.docs.len(),
                distinct_values,
                values: windowed_vals,
                remaining_values,
                range,
                collections,
            });
        }

        if !bucket_list.is_empty() {
            discovered_keys.push(DiscoveredMetadataKey {
                key,
                documents: key_docs.len(),
                types: bucket_list,
            });
        }
    }

    // Sort keys by coverage descending, then alphabetically
    discovered_keys.sort_by(|a, b| {
        b.documents
            .cmp(&a.documents)
            .then_with(|| a.key.cmp(&b.key))
    });

    let total_keys = discovered_keys.len();
    let keys: Vec<DiscoveredMetadataKey> = discovered_keys
        .into_iter()
        .skip(key_offset)
        .take(key_limit)
        .collect();
    let shown_keys_end = (key_offset + keys.len()).min(total_keys);
    let remaining_keys = total_keys.saturating_sub(shown_keys_end);

    Ok(MetadataDiscoveryResult {
        documents: total_in_scope,
        filtered_documents,
        total_keys,
        remaining_keys,
        keys,
    })
}
