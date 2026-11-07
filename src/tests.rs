
use crate::api::search::aggregate_matches;
use crate::api::skills::{parse_uploaded_skill, skill_count_http_response};
use crate::types::SkillMatch;
use crate::*;

fn test_skill(name: &str) -> Skill {
    Skill::new(
        name,
        name,
        format!("{name} description"),
        format!("Use {name}."),
        Vec::new(),
    )
}

fn response_json(response: &http::Response) -> serde_json::Value {
    serde_json::from_slice(&response.body).unwrap()
}

#[lyquid_test::test]
fn allocates_a_guest_string() {
    let greeting = String::from("hello");
    assert_eq!(greeting.as_str(), "hello");
}

#[lyquid_test::test]
fn parses_a_single_direct_serde_skill() {
    let skill = Skill::new(
        "direct-serde",
        "direct-serde",
        "Direct serde description.",
        "Use direct serde.",
        vec![crate::skill::SkillResource::new("nested/a.txt", "contents").unwrap()],
    );
    let body = serde_json::to_vec(&skill).unwrap();

    let (parsed, digest) = parse_uploaded_skill(Some(&body)).unwrap();

    assert_eq!(parsed, skill);
    assert_eq!(digest, skill.digest().unwrap().to_string());
}

#[lyquid_test::test]
fn parses_a_single_transport_skill() {
    let skill = test_skill("transport-serde");
    let body = skill.to_json().unwrap();

    let (parsed, digest) = parse_uploaded_skill(Some(body.as_bytes())).unwrap();

    assert_eq!(parsed, skill);
    assert_eq!(digest, skill.digest().unwrap().to_string());
}

#[lyquid_test::test]
fn rejects_skill_arrays_and_non_objects() {
    let skills = vec![test_skill("one"), test_skill("two")];
    let array = serde_json::to_vec(&skills).unwrap();
    let error = parse_uploaded_skill(Some(&array)).unwrap_err();
    assert!(error.contains("JSON object"));

    let error = parse_uploaded_skill(Some(b"null")).unwrap_err();
    assert!(error.contains("JSON object"));
}

#[lyquid_test::test]
fn rejects_missing_and_malformed_request_bodies() {
    let error = parse_uploaded_skill(None).unwrap_err();
    assert!(error.contains("required"));

    let error = parse_uploaded_skill(Some(b"{")).unwrap_err();
    assert!(error.contains("invalid skill JSON"));
}

#[lyquid_test::test]
fn rejects_invalid_transport_skills() {
    let body = serde_json::json!({
        "name": "invalid",
        "description": "Invalid skill.",
        "instructions": "Use it.",
        "resources": [{"path": "../escape", "contents": "nope"}],
    })
    .to_string();

    let error = parse_uploaded_skill(Some(body.as_bytes())).unwrap_err();

    assert!(error.contains("skill is invalid"));
}

#[lyquid_test::test]
fn parses_a_search_request_with_a_default_k() {
    let request = parse_search_endpoint_request(Some(br#"{"query":"rust async"}"#)).unwrap();

    assert_eq!(request.query, "rust async");
    assert_eq!(request.k, 10);
}

#[lyquid_test::test]
fn parses_a_search_request_with_an_explicit_k() {
    let request = parse_search_endpoint_request(Some(br#"{"query":"rust async","k":5}"#)).unwrap();

    assert_eq!(request.query, "rust async");
    assert_eq!(request.k, 5);
}

#[lyquid_test::test]
fn rejects_missing_empty_and_malformed_search_requests() {
    let error = parse_search_endpoint_request(None).unwrap_err();
    assert!(error.contains("required"));

    let error = parse_search_endpoint_request(Some(br#"{"query":"   "}"#)).unwrap_err();
    assert!(error.contains("must not be empty"));

    let error = parse_search_endpoint_request(Some(b"{")).unwrap_err();
    assert!(error.contains("invalid search request JSON"));
}

#[lyquid_test::test]
fn query_text_round_trips_through_certified_input_encoding() {
    let query =
        SearchQuery::new(b"skill", QueryParams { k: 5 }, 1, Address::from([1u8; 20])).unwrap();
    let encoded = encode_by_fields!(query: SearchQuery = query.clone());
    let decoded = decode_by_fields!(&encoded, query: SearchQuery).unwrap();

    assert_eq!(decoded.query.query_text, "skill");
    assert_eq!(decoded.query, query);
}

#[lyquid_test::test]
fn search_accepted_response_contains_the_query_id() {
    let query_id = blake3::hash(b"query");
    let response = search_accepted_response(query_id);
    let body = response_json(&response);

    assert_eq!(response.status, 202);
    assert_eq!(body["status"].as_str(), Some("submitted"));
    assert_eq!(
        body["query_id"].as_str(),
        Some(query_id.to_string().as_str())
    );
}

#[lyquid_test::test]
fn parses_query_ids_from_json_bodies() {
    let query_id = blake3::hash(b"query");

    let body = format!(r#"{{"query_id":"{query_id}"}}"#);
    assert_eq!(
        parse_query_id_request(Some(body.as_bytes())).unwrap(),
        query_id
    );

    let body = format!(r#"{{"query_id":"0x{query_id}"}}"#);
    assert_eq!(
        parse_query_id_request(Some(body.as_bytes())).unwrap(),
        query_id
    );
}

#[lyquid_test::test]
fn rejects_missing_and_malformed_query_ids() {
    let error = parse_query_id_request(None).unwrap_err();
    assert!(error.contains("required"));

    let error = parse_query_id_request(Some(br#"{"query_id":"0x1234"}"#)).unwrap_err();
    assert!(error.contains("invalid query_id"));

    let error = parse_query_id_request(Some(b"{")).unwrap_err();
    assert!(error.contains("invalid query id request JSON"));
}

#[lyquid_test::test]
fn search_status_response_reports_progress() {
    let query_id = blake3::hash(b"query");
    let response = search_status_response(query_id, false, 2, 4);
    let body = response_json(&response);

    assert_eq!(response.status, 200);
    assert_eq!(body["status"].as_str(), Some("pending"));
    assert_eq!(body["completed"].as_bool(), Some(false));
    assert_eq!(body["received_results"].as_u64(), Some(2));
    assert_eq!(body["expected_results"].as_u64(), Some(4));
}

#[lyquid_test::test]
fn search_pending_response_returns_409() {
    let query_id = blake3::hash(b"query");
    let response = search_pending_response(query_id, 1, 3);
    let body = response_json(&response);

    assert_eq!(response.status, 409);
    assert_eq!(body["completed"].as_bool(), Some(false));
    assert_eq!(body["received_results"].as_u64(), Some(1));
    assert_eq!(body["expected_results"].as_u64(), Some(3));
    assert_eq!(body["results"].as_array().map(Vec::len), Some(0));
}

#[lyquid_test::test]
fn search_results_response_contains_results() {
    let query_id = blake3::hash(b"query");
    let response = search_results_response(query_id, &[]);
    let body = response_json(&response);

    assert_eq!(response.status, 200);
    assert_eq!(body["completed"].as_bool(), Some(true));
    assert_eq!(body["results"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["aggregated"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["verified_results"].as_u64(), Some(0));
    assert_eq!(body["rejected_results"].as_u64(), Some(0));
}

#[lyquid_test::test]
fn aggregate_matches_keeps_best_score_and_sorts() {
    let query_id = blake3::hash(b"query");
    let digest_a = blake3::hash(b"skill-a");
    let digest_b = blake3::hash(b"skill-b");
    let digest_c = blake3::hash(b"skill-c");
    let first = SearchResult::new(
        query_id,
        vec![
            SkillMatch {
                skill_digest: digest_a,
                bm25_score: 2,
            },
            SkillMatch {
                skill_digest: digest_c,
                bm25_score: 4,
            },
        ],
        1,
        Address::from([1u8; 20]),
    )
    .unwrap();
    let second = SearchResult::new(
        query_id,
        vec![
            SkillMatch {
                skill_digest: digest_a,
                bm25_score: 9,
            },
            SkillMatch {
                skill_digest: digest_b,
                bm25_score: 5,
            },
        ],
        2,
        Address::from([2u8; 20]),
    )
    .unwrap();

    let aggregated = aggregate_matches(&[&first, &second]);
    assert_eq!(aggregated.len(), 3);
    assert_eq!(aggregated[0].skill_digest, digest_a);
    assert_eq!(aggregated[0].bm25_score, 9);
    assert_eq!(aggregated[1].skill_digest, digest_b);
    assert_eq!(aggregated[1].bm25_score, 5);
    assert_eq!(aggregated[2].skill_digest, digest_c);
    assert_eq!(aggregated[2].bm25_score, 4);
}

#[lyquid_test::test]
fn count_response_reflects_manager_contents() {
    let manager = SkillManager::from_skills([test_skill("counted")]).unwrap();
    let response = skill_count_http_response(manager.len());
    let body = response_json(&response);

    assert_eq!(response.status, 200);
    assert_eq!(body["count"].as_u64(), Some(1));
}

#[lyquid_test::test]
fn list_response_contains_name_description_and_digest() {
    let skill = test_skill("listed");
    let digest = skill.digest().unwrap().to_string();
    let manager = SkillManager::from_skills([skill.clone()]).unwrap();

    let body: serde_json::Value = serde_json::from_str(&skill_list_json(&manager)).unwrap();
    let entries = body.as_array().unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"].as_str(), Some(skill.name.as_str()));
    assert_eq!(
        entries[0]["description"].as_str(),
        Some(skill.description.as_str())
    );
    assert_eq!(entries[0]["digest"].as_str(), Some(digest.as_str()));
    assert_eq!(entries[0].as_object().unwrap().len(), 3);
}
