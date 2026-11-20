//! Request, response, and aggregation helpers for the search HTTP API.

use lyquid::prelude::*;

use crate::types::merkle::serialize_hash;
use crate::types::{Bm25Score, SearchResult, parse_hash};
use crate::verify;

use super::http::http_json_response;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PeerSearchReply {
    pub(crate) result: SearchResult,
    /// Canonical skill bytes for the result's matches, in the same order.
    pub(crate) skill_bodies: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SearchReply {
    pub(crate) replies: Vec<PeerSearchReply>,
}

pub(crate) struct SearchReplyCache {
    pub(crate) expected: usize,
    pub(crate) replies: Vec<PeerSearchReply>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct SearchEndpointRequest {
    pub(crate) query: String,
    #[serde(default = "default_results_per_peer")]
    pub(crate) k: u64,
}

pub(crate) fn default_results_per_peer() -> u64 {
    10
}

pub(crate) fn parse_search_endpoint_request(
    body: Option<&[u8]>,
) -> Result<SearchEndpointRequest, String> {
    let body = body.ok_or_else(|| "request body is required".to_owned())?;
    let request: SearchEndpointRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid search request JSON: {error}"))?;

    if request.query.trim().is_empty() {
        return Err("query must not be empty".to_owned());
    }

    Ok(request)
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct QueryIdRequest {
    query_id: String,
}

pub(crate) fn parse_query_id_request(body: Option<&[u8]>) -> Result<Hash, String> {
    let body = body.ok_or_else(|| "request body is required".to_owned())?;
    let request: QueryIdRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid query id request JSON: {error}"))?;
    parse_hash(request.query_id.trim()).map_err(|error| format!("invalid query_id: {error}"))
}

pub(crate) fn search_accepted_response(query_id: Hash) -> http::Response {
    let body = serde_json::json!({
        "status": "submitted",
        "query_id": query_id.to_string(),
    })
    .to_string();
    http_json_response(202, &body)
}

pub(crate) fn search_status_response(
    query_id: Hash,
    completed: bool,
    received_results: usize,
    expected_results: usize,
    timed_out: bool,
) -> http::Response {
    let body = serde_json::json!({
        "status": if completed { "completed" } else { "pending" },
        "query_id": query_id.to_string(),
        "completed": completed,
        "timed_out": timed_out,
        "received_results": received_results,
        "expected_results": expected_results,
    })
    .to_string();
    http_json_response(200, &body)
}

pub(crate) fn search_pending_response(
    query_id: Hash,
    received_results: usize,
    expected_results: usize,
) -> http::Response {
    let body = serde_json::json!({
        "status": "pending",
        "query_id": query_id.to_string(),
        "completed": false,
        "received_results": received_results,
        "expected_results": expected_results,
        "results": [],
    })
    .to_string();
    http_json_response(409, &body)
}

pub(crate) fn search_results_response(
    query_id: Hash,
    results: &[&SearchResult],
    timed_out: bool,
) -> http::Response {
    let verified: Vec<&SearchResult> = results
        .iter()
        .copied()
        .filter(|result| verify::verify_signed(*result))
        .collect();
    let rejected_results = results.len() - verified.len();
    let aggregated = aggregate_matches(&verified);

    let body = serde_json::json!({
        "status": "completed",
        "query_id": query_id.to_string(),
        "completed": true,
        "timed_out": timed_out,
        "verified_results": verified.len(),
        "rejected_results": rejected_results,
        "results": verified,
        "aggregated": aggregated,
    })
    .to_string();
    http_json_response(200, &body)
}

/// A locally aggregated search match, returned to the client for a completed
/// query.
///
/// Unlike [`SkillMatch`](crate::types::SkillMatch), which is stored in consensus
/// state, this type only exists in the initiating node's HTTP response. `origin`
/// is the (signed) sender of the [`SearchResult`] that supplied the best score
/// for this skill, which lets clients confirm that results came from multiple
/// peers. It is derived locally from the result's authenticated sender, so it is
/// not part of the on-chain result and never round-trips through a peer's JSON.
///
/// Note that BM25 scores are computed against each peer's local corpus, so
/// `origin` identifies who reported the retained score, not the best node for
/// that skill network-wide.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct AggregatedMatch {
    /// Digest of the matched skill.
    #[serde(serialize_with = "serialize_hash")]
    pub(crate) skill_digest: Hash,
    /// Best BM25 score seen for this skill across all verified peer results.
    pub(crate) bm25_score: Bm25Score,
    /// Node ID of the peer that supplied the best score, omitted from the
    /// response when it is not known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) origin: Option<String>,
}

// Deduplicates matches by skill digest, keeping the best score seen for each
// skill, and orders them by descending score then digest.
pub(crate) fn aggregate_matches(results: &[&SearchResult]) -> Vec<AggregatedMatch> {
    // (score, origin) so we track which peer provided the best score.
    let mut best_scores: HashMap<Hash, (Bm25Score, Option<String>)> = new_hashmap();
    for result in results {
        for skill_match in &result.matches {
            let entry = best_scores
                .entry(skill_match.skill_digest)
                .or_insert_with(|| (skill_match.bm25_score, None));
            if skill_match.bm25_score > entry.0 || entry.1.is_none() {
                entry.0 = skill_match.bm25_score;
                entry.1 = Some(result.sender.to_string());
            }
        }
    }

    let mut aggregated: Vec<AggregatedMatch> = best_scores
        .into_iter()
        .map(|(skill_digest, (bm25_score, origin))| AggregatedMatch {
            skill_digest,
            bm25_score,
            origin,
        })
        .collect();
    aggregated.sort_by(|left, right| {
        right.bm25_score.cmp(&left.bm25_score).then_with(|| {
            left.skill_digest
                .as_bytes()
                .cmp(right.skill_digest.as_bytes())
        })
    });
    aggregated
}
