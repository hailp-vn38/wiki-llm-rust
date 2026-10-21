use crate::error::WikiResult;
use crate::search::fts::search_fts_bm25;
use crate::search::vector::{search_vector, EmbeddingProvider, RerankProvider};
use crate::search::{
    HybridQueryRequest, RrfContribution, RrfExplainTrace, SearchExplainTrace, SearchOptions,
    SearchResultItem, SubQuery, SubQueryType,
};
use rusqlite::Connection;
use std::collections::HashMap;

const RRF_K: f32 = 60.0;
const TOP_1_BONUS: f32 = 0.05;
const TOP_2_3_BONUS: f32 = 0.02;

struct RankedDocAccumulator {
    item: SearchResultItem,
    fts_scores: Vec<f32>,
    vector_scores: Vec<f32>,
    base_rrf_score: f32,
    best_rank: usize,
    contributions: Vec<RrfContribution>,
}

/// Execute the full QMD Hybrid Search Pipeline:
/// 1. Route typed sub-queries (`lex` -> FTS5 BM25, `vec`/`hyde` -> Vector Search).
///    First sub-query gets 2x weight; subsequent sub-queries get 1x weight.
/// 2. Reciprocal Rank Fusion (`k = 60`): `sum(weight / (k + rank + 1))`
/// 3. Top-Rank Bonus: `+0.05` if Rank #1 in any list, `+0.02` if Rank #2 or #3.
/// 4. Top-K selection (`candidate_limit`, default 40) -> Optional Cross-Encoder Reranking.
/// 5. Position-Aware Blending:
///    - RRF Rank 1-3:  75% RRF + 25% Reranker
///    - RRF Rank 4-10: 60% RRF + 40% Reranker
///    - RRF Rank 11+:  40% RRF + 60% Reranker
pub fn execute_hybrid_query(
    conn: &Connection,
    embedder: &dyn EmbeddingProvider,
    reranker: Option<&dyn RerankProvider>,
    req: &HybridQueryRequest,
) -> WikiResult<Vec<SearchResultItem>> {
    let candidate_limit = req.candidate_limit.unwrap_or(40).max(req.limit);

    // Build effective sub-queries list
    let mut sub_queries: Vec<SubQuery> = req.searches.clone();
    if sub_queries.is_empty() {
        if let Some(ref q) = req.query {
            // Send original query to bothLex and Vec, plus optional LLM expansion
            sub_queries.push(SubQuery {
                query_type: SubQueryType::Lex,
                query: q.clone(),
            });
            sub_queries.push(SubQuery {
                query_type: SubQueryType::Vec,
                query: q.clone(),
            });
            if let Some(rr) = reranker {
                if let Ok(expanded) = rr.expand_query(q, req.intent.as_deref()) {
                    for exp in expanded {
                        sub_queries.push(exp);
                    }
                }
            }
        }
    }

    if sub_queries.is_empty() {
        return Ok(Vec::new());
    }

    let backend_opts = SearchOptions {
        collections: req.collections.clone(),
        filter: req.filter.clone(),
        limit: candidate_limit,
        min_score: 0.0,
    };

    let mut accumulators: HashMap<String, RankedDocAccumulator> = HashMap::new();

    for (sq_idx, sq) in sub_queries.iter().enumerate() {
        let weight = if sq_idx == 0 { 2.0f32 } else { 1.0f32 };
        let query_type_label = if sq_idx == 0 {
            "original".to_string()
        } else {
            sq.query_type.as_str().to_string()
        };

        match sq.query_type {
            SubQueryType::Lex => {
                let fts_list = search_fts_bm25(conn, &sq.query, &backend_opts)?;
                for (rank, item) in fts_list.into_iter().enumerate() {
                    let rrf_contrib = weight / (RRF_K + (rank as f32) + 1.0);
                    let entry = accumulators
                        .entry(item.rel_path.clone())
                        .or_insert_with(|| RankedDocAccumulator {
                            item: item.clone(),
                            fts_scores: Vec::new(),
                            vector_scores: Vec::new(),
                            base_rrf_score: 0.0,
                            best_rank: usize::MAX,
                            contributions: Vec::new(),
                        });
                    entry.fts_scores.push(item.score);
                    entry.base_rrf_score += rrf_contrib;
                    entry.best_rank = entry.best_rank.min(rank);
                    entry.contributions.push(RrfContribution {
                        source: "fts".to_string(),
                        query_type: query_type_label.clone(),
                        query: sq.query.clone(),
                        rank: rank + 1,
                        weight,
                        backend_score: item.score,
                        rrf_contribution: rrf_contrib,
                    });
                }
            }
            SubQueryType::Vec | SubQueryType::Hyde => {
                let vec_list = search_vector(conn, embedder, &sq.query, &backend_opts)?;
                for (rank, item) in vec_list.into_iter().enumerate() {
                    let rrf_contrib = weight / (RRF_K + (rank as f32) + 1.0);
                    let entry = accumulators
                        .entry(item.rel_path.clone())
                        .or_insert_with(|| RankedDocAccumulator {
                            item: item.clone(),
                            fts_scores: Vec::new(),
                            vector_scores: Vec::new(),
                            base_rrf_score: 0.0,
                            best_rank: usize::MAX,
                            contributions: Vec::new(),
                        });
                    entry.vector_scores.push(item.score);
                    entry.base_rrf_score += rrf_contrib;
                    entry.best_rank = entry.best_rank.min(rank);
                    entry.contributions.push(RrfContribution {
                        source: "vector".to_string(),
                        query_type: query_type_label.clone(),
                        query: sq.query.clone(),
                        rank: rank + 1,
                        weight,
                        backend_score: item.score,
                        rrf_contribution: rrf_contrib,
                    });
                }
            }
        }
    }

    if accumulators.is_empty() {
        return Ok(Vec::new());
    }

    // Compute total RRF score with Top-Rank Bonus
    let mut fused: Vec<(f32, f32, RankedDocAccumulator)> = accumulators
        .into_values()
        .map(|acc| {
            let bonus = if acc.best_rank == 0 {
                TOP_1_BONUS
            } else if acc.best_rank <= 2 {
                TOP_2_3_BONUS
            } else {
                0.0
            };
            let total_rrf = acc.base_rrf_score + bonus;
            (total_rrf, bonus, acc)
        })
        .collect();

    fused.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.2.item.rel_path.cmp(&b.2.item.rel_path))
    });

    fused.truncate(candidate_limit);

    // Normalize RRF score to [0.0, 1.0] relative to theoretical max or top candidate
    let max_rrf = fused.first().map(|x| x.0).unwrap_or(1.0).max(0.05);

    // Optional Reranker step + Position-Aware Blending
    let primary_query = req
        .query
        .as_deref()
        .or_else(|| sub_queries.first().map(|s| s.query.as_str()))
        .unwrap_or("");

    let rerank_scores: Option<Vec<f32>> = if req.rerank {
        if let Some(rr) = reranker {
            let candidate_texts: Vec<String> = fused
                .iter()
                .map(|(_, _, acc)| format!("{} | {}", acc.item.title, acc.item.snippet))
                .collect();
            rr.rerank(primary_query, &candidate_texts).ok()
        } else {
            None
        }
    } else {
        None
    };

    let mut final_results = Vec::with_capacity(fused.len());
    for (idx, (total_rrf, bonus, mut acc)) in fused.into_iter().enumerate() {
        let rrf_rank = idx + 1;
        // Scale RRF score smoothly into [0.0, 1.0]
        let rrf_norm = (total_rrf / max_rrf).clamp(0.0, 1.0);

        let (rrf_weight, final_score, rerank_score_opt) = if let Some(ref r_scores) = rerank_scores
        {
            let r_score = r_scores
                .get(idx)
                .copied()
                .unwrap_or(rrf_norm)
                .clamp(0.0, 1.0);
            let w_rrf = if rrf_rank <= 3 {
                0.75f32
            } else if rrf_rank <= 10 {
                0.60f32
            } else {
                0.40f32
            };
            let blended = (w_rrf * rrf_norm + (1.0 - w_rrf) * r_score).clamp(0.0, 1.0);
            (w_rrf, blended, Some(r_score))
        } else {
            (1.0f32, rrf_norm, None)
        };

        if final_score < req.min_score {
            continue;
        }

        acc.item.score = final_score;
        acc.item.raw_backend_score = total_rrf;

        if req.explain {
            acc.item.explain = Some(SearchExplainTrace {
                fts_scores: acc.fts_scores,
                vector_scores: acc.vector_scores,
                reranker_score: rerank_score_opt,
                rrf: RrfExplainTrace {
                    rank: rrf_rank,
                    weight: rrf_weight,
                    base_score: acc.base_rrf_score,
                    top_rank_bonus: bonus,
                    total_score: total_rrf,
                    contributions: acc.contributions,
                },
            });
        }

        final_results.push(acc.item);
    }

    final_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    final_results.truncate(req.limit);

    Ok(final_results)
}
