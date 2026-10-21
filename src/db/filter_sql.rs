use crate::error::{WikiError, WikiResult};
use crate::markdown::frontmatter::{validate_qmd_meta_value, TypedMetaValue};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// QMD recursive Metadata Filter AST (`--filter` and `--match`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataFilter {
    /// Logical group: `{"operator": "and" | "or", "operands": [...]}`
    Group {
        operator: LogicalOperator,
        operands: Vec<MetadataFilter>,
    },
    /// Negation: `{"operator": "not", "operand": {...}}`
    Not {
        operator: NotOperator,
        operand: Box<MetadataFilter>,
    },
    /// Field condition: `{"field": "status", "operator": "eq", "value": "published", "caseInsensitive": false}`
    Condition(MetadataCondition),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogicalOperator {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotOperator {
    Not,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataCondition {
    pub field: String,
    pub operator: ComparisonOperator,
    pub value: serde_json::Value,
    #[serde(default, rename = "caseInsensitive")]
    pub case_insensitive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComparisonOperator {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    Nin,
    All,
    Contains,
    Prefix,
    Suffix,
    Type,
    Exists,
}

/// Evaluate a `MetadataFilter` against all indexed documents in the database,
/// returning the set of matching `document_id`s.
pub fn resolve_matching_document_ids(
    conn: &Connection,
    filter: &MetadataFilter,
) -> WikiResult<HashSet<i64>> {
    let mut stmt = conn.prepare("SELECT id, metadata_json FROM documents")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let meta_json: String = row.get(1)?;
        Ok((id, meta_json))
    })?;

    let mut matching_ids = HashSet::new();
    for row in rows {
        let (doc_id, meta_json) = row?;
        let raw_map: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&meta_json).unwrap_or_default();
        let mut typed_map = BTreeMap::new();
        for (k, v) in raw_map {
            if let Some(tv) = validate_qmd_meta_value(&v) {
                typed_map.insert(k, tv);
            }
        }
        if eval_filter_on_doc(filter, &typed_map)? {
            matching_ids.insert(doc_id);
        }
    }

    Ok(matching_ids)
}

/// Evaluate a QMD `MetadataFilter` directly against a document's typed metadata map.
pub fn eval_filter_on_doc(
    filter: &MetadataFilter,
    doc_meta: &BTreeMap<String, TypedMetaValue>,
) -> WikiResult<bool> {
    match filter {
        MetadataFilter::Group { operator, operands } => {
            if operands.is_empty() {
                return Err(WikiError::InvalidMetadataFilter(
                    "Logical group must have at least 1 operand".to_string(),
                ));
            }
            match operator {
                LogicalOperator::And => {
                    for op in operands {
                        if !eval_filter_on_doc(op, doc_meta)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                LogicalOperator::Or => {
                    for op in operands {
                        if eval_filter_on_doc(op, doc_meta)? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
            }
        }
        MetadataFilter::Not { operand, .. } => Ok(!eval_filter_on_doc(operand, doc_meta)?),
        MetadataFilter::Condition(cond) => eval_condition_on_doc(cond, doc_meta),
    }
}

fn eval_condition_on_doc(
    cond: &MetadataCondition,
    doc_meta: &BTreeMap<String, TypedMetaValue>,
) -> WikiResult<bool> {
    let field_val = doc_meta.get(&cond.field);

    if cond.operator == ComparisonOperator::Exists {
        let expected = cond.value.as_bool().ok_or_else(|| {
            WikiError::InvalidMetadataFilter(
                "`exists` operator requires a boolean value".to_string(),
            )
        })?;
        return Ok(field_val.is_some() == expected);
    }

    let Some(val) = field_val else {
        // Missing keys do not match any condition other than `exists: false`
        return Ok(false);
    };

    if cond.operator == ComparisonOperator::Type {
        let expected_type = cond.value.as_str().ok_or_else(|| {
            WikiError::InvalidMetadataFilter(
                "`type` operator requires 'string', 'number', or 'boolean'".to_string(),
            )
        })?;
        return Ok(val.value_type() == expected_type);
    }

    let ci = cond.case_insensitive;

    match cond.operator {
        ComparisonOperator::Eq => Ok(match_scalar_or_any(val, &cond.value, ci, ScalarCmp::Eq)),
        ComparisonOperator::Ne => Ok(match_ne(val, &cond.value, ci)),
        ComparisonOperator::Gt => Ok(match_scalar_or_any(val, &cond.value, ci, ScalarCmp::Gt)),
        ComparisonOperator::Gte => Ok(match_scalar_or_any(val, &cond.value, ci, ScalarCmp::Gte)),
        ComparisonOperator::Lt => Ok(match_scalar_or_any(val, &cond.value, ci, ScalarCmp::Lt)),
        ComparisonOperator::Lte => Ok(match_scalar_or_any(val, &cond.value, ci, ScalarCmp::Lte)),
        ComparisonOperator::Contains => Ok(match_text_op(val, &cond.value, ci, TextOp::Contains)),
        ComparisonOperator::Prefix => Ok(match_text_op(val, &cond.value, ci, TextOp::Prefix)),
        ComparisonOperator::Suffix => Ok(match_text_op(val, &cond.value, ci, TextOp::Suffix)),
        ComparisonOperator::In => {
            let Some(arr) = cond.value.as_array() else {
                return Err(WikiError::InvalidMetadataFilter(
                    "`in` operator requires an array value".to_string(),
                ));
            };
            Ok(arr
                .iter()
                .any(|item| match_scalar_or_any(val, item, ci, ScalarCmp::Eq)))
        }
        ComparisonOperator::Nin => {
            let Some(arr) = cond.value.as_array() else {
                return Err(WikiError::InvalidMetadataFilter(
                    "`nin` operator requires an array value".to_string(),
                ));
            };
            // Must match type of at least one operand and not equal any operand
            if arr.is_empty() {
                return Ok(false);
            }
            Ok(arr.iter().all(|item| match_ne(val, item, ci)))
        }
        ComparisonOperator::All => {
            let Some(arr) = cond.value.as_array() else {
                return Err(WikiError::InvalidMetadataFilter(
                    "`all` operator requires an array value".to_string(),
                ));
            };
            if arr.is_empty() {
                return Ok(false);
            }
            Ok(arr
                .iter()
                .all(|item| match_scalar_or_any(val, item, ci, ScalarCmp::Eq)))
        }
        ComparisonOperator::Type | ComparisonOperator::Exists => unreachable!(),
    }
}

#[derive(Clone, Copy)]
enum ScalarCmp {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Clone, Copy)]
enum TextOp {
    Contains,
    Prefix,
    Suffix,
}

fn fold_ci(s: &str, ci: bool) -> String {
    if ci {
        s.to_ascii_lowercase()
    } else {
        s.to_string()
    }
}

fn compare_str(a: &str, b: &str, ci: bool, cmp: ScalarCmp) -> bool {
    let lhs = fold_ci(a, ci);
    let rhs = fold_ci(b, ci);
    match cmp {
        ScalarCmp::Eq => lhs == rhs,
        ScalarCmp::Gt => lhs > rhs,
        ScalarCmp::Gte => lhs >= rhs,
        ScalarCmp::Lt => lhs < rhs,
        ScalarCmp::Lte => lhs <= rhs,
    }
}

fn compare_num(a: f64, b: f64, cmp: ScalarCmp) -> bool {
    match cmp {
        ScalarCmp::Eq => (a - b).abs() < f64::EPSILON,
        ScalarCmp::Gt => a > b,
        ScalarCmp::Gte => a >= b,
        ScalarCmp::Lt => a < b,
        ScalarCmp::Lte => a <= b,
    }
}

fn match_scalar_or_any(
    doc_val: &TypedMetaValue,
    target: &serde_json::Value,
    ci: bool,
    cmp: ScalarCmp,
) -> bool {
    match (doc_val, target) {
        (TypedMetaValue::String(s), serde_json::Value::String(t)) => compare_str(s, t, ci, cmp),
        (TypedMetaValue::StringArray(arr), serde_json::Value::String(t)) => {
            arr.iter().any(|s| compare_str(s, t, ci, cmp))
        }
        (TypedMetaValue::Number(n), serde_json::Value::Number(t)) => t
            .as_f64()
            .map(|tf| compare_num(*n, tf, cmp))
            .unwrap_or(false),
        (TypedMetaValue::NumberArray(arr), serde_json::Value::Number(t)) => t
            .as_f64()
            .map(|tf| arr.iter().any(|n| compare_num(*n, tf, cmp)))
            .unwrap_or(false),
        (TypedMetaValue::Bool(b), serde_json::Value::Bool(t)) => match cmp {
            ScalarCmp::Eq => b == t,
            _ => false,
        },
        (TypedMetaValue::BoolArray(arr), serde_json::Value::Bool(t)) => match cmp {
            ScalarCmp::Eq => arr.iter().any(|b| b == t),
            _ => false,
        },
        _ => false,
    }
}

fn match_ne(doc_val: &TypedMetaValue, target: &serde_json::Value, ci: bool) -> bool {
    match (doc_val, target) {
        (TypedMetaValue::String(s), serde_json::Value::String(t)) => {
            !compare_str(s, t, ci, ScalarCmp::Eq)
        }
        (TypedMetaValue::StringArray(arr), serde_json::Value::String(t)) => {
            !arr.is_empty() && arr.iter().all(|s| !compare_str(s, t, ci, ScalarCmp::Eq))
        }
        (TypedMetaValue::Number(n), serde_json::Value::Number(t)) => t
            .as_f64()
            .map(|tf| !compare_num(*n, tf, ScalarCmp::Eq))
            .unwrap_or(false),
        (TypedMetaValue::NumberArray(arr), serde_json::Value::Number(t)) => t
            .as_f64()
            .map(|tf| !arr.is_empty() && arr.iter().all(|n| !compare_num(*n, tf, ScalarCmp::Eq)))
            .unwrap_or(false),
        (TypedMetaValue::Bool(b), serde_json::Value::Bool(t)) => b != t,
        (TypedMetaValue::BoolArray(arr), serde_json::Value::Bool(t)) => {
            !arr.is_empty() && arr.iter().all(|b| b != t)
        }
        _ => false,
    }
}

fn match_text_op(
    doc_val: &TypedMetaValue,
    target: &serde_json::Value,
    ci: bool,
    op: TextOp,
) -> bool {
    let Some(t_str) = target.as_str() else {
        return false;
    };
    let rhs = fold_ci(t_str, ci);
    let check = |s: &str| {
        let lhs = fold_ci(s, ci);
        match op {
            TextOp::Contains => lhs.contains(&rhs),
            TextOp::Prefix => lhs.starts_with(&rhs),
            TextOp::Suffix => lhs.ends_with(&rhs),
        }
    };

    match doc_val {
        TypedMetaValue::String(s) => check(s),
        TypedMetaValue::StringArray(arr) => arr.iter().any(|s| check(s)),
        _ => false,
    }
}
