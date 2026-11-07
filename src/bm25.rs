//! Deterministic BM25 search over the locally available skills.
//!
//! The [`khive_bm25`] index itself is not usable in the wasm guest because it
//! records timings with [`std::time::Instant`], which is unimplemented on
//! `wasm32-unknown-unknown`. We therefore reuse its tokenizer and fixed-point
//! score type but compute the BM25 sum with explicit scalar `f64` arithmetic.
//! Keeping the evaluation order fixed makes the scores reproducible across
//! peers and platforms.

// TODO: use khive_bm25::Bm25Index once its Instant-based metrics can be disabled (or vendor the tokenizer and score types) so this scorer cannot drift from upstream.

use khive_bm25::{DeterministicScore, tokenize};
use lyquid::prelude::{Hash, HashMap, new_hashmap, new_hashset};

use crate::skill::Skill;
use crate::types::{MAX_MATCHES, SkillMatch};

/// Term saturation parameter.
const K1: f64 = 1.2;
/// Length normalization parameter.
const B: f64 = 0.75;

/// A skill prepared for scoring.
struct IndexedSkill {
    digest: Hash,
    tokens: Vec<String>,
    term_freqs: HashMap<String, u32>,
}

impl IndexedSkill {
    fn new(skill: &Skill, digest: Hash) -> Self {
        let tokens = tokenize(&skill.search_text());
        let mut term_freqs = new_hashmap();
        for token in &tokens {
            *term_freqs.entry(token.clone()).or_insert(0) += 1;
        }
        Self {
            digest,
            tokens,
            term_freqs,
        }
    }

    fn len(&self) -> usize {
        self.tokens.len()
    }
}

/// Searches `skills` for the `top_k` best matches to `query`.
///
/// The search runs in two stages, matching the protocol's verification rules:
///
/// 1. Every distinct skill in `skills` is scored with BM25 to select the
///    `top_k` candidates.
/// 2. The selected candidates alone are used as the corpus and are scored
///    again. These canonical scores are what [`SkillMatch`] carries, so a peer
///    can recompute them from the returned matches without needing the full
///    skill library.
///
/// Skills are deduplicated by digest, and skills whose canonical encoding is
/// invalid (or that tokenize to nothing) are skipped. Results are ordered by
/// descending canonical score, with ties broken by ascending skill digest,
/// and are capped at [`MAX_MATCHES`] so the output can always be submitted as
/// a valid on-chain result.
pub fn search(query: &str, top_k: usize, skills: &[Skill]) -> Vec<SkillMatch> {
    let top_k = top_k.min(MAX_MATCHES);
    if top_k == 0 || skills.is_empty() {
        return Vec::new();
    }

    let indexed = index_skills(skills);
    if indexed.is_empty() {
        return Vec::new();
    }

    // Stage 1: rank the full local corpus and keep the top-k candidates.
    let all: Vec<&IndexedSkill> = indexed.iter().collect();
    let scores = score(query, &all);
    let mut ranked: Vec<usize> = (0..indexed.len())
        .filter(|&position| scores[position] > 0)
        .collect();
    ranked.sort_by(|&left, &right| {
        scores[right].cmp(&scores[left]).then_with(|| {
            indexed[left]
                .digest
                .as_bytes()
                .cmp(indexed[right].digest.as_bytes())
        })
    });
    ranked.truncate(top_k);
    if ranked.is_empty() {
        return Vec::new();
    }

    // Stage 2: recompute canonical scores using only the selected skills.
    let selected: Vec<&IndexedSkill> = ranked.iter().map(|&position| &indexed[position]).collect();
    let canonical_scores = score(query, &selected);

    let mut matches: Vec<SkillMatch> = selected
        .iter()
        .zip(canonical_scores)
        .map(|(indexed, bm25_score)| SkillMatch {
            skill_digest: indexed.digest,
            bm25_score,
        })
        .collect();

    matches.sort_by(|left, right| {
        right.bm25_score.cmp(&left.bm25_score).then_with(|| {
            left.skill_digest
                .as_bytes()
                .cmp(right.skill_digest.as_bytes())
        })
    });
    matches
}

/// Deduplicates and tokenizes the input skills.
fn index_skills(skills: &[Skill]) -> Vec<IndexedSkill> {
    let mut indexed = Vec::new();
    let mut seen = new_hashset();
    for skill in skills {
        let Ok(digest) = skill.digest() else {
            continue;
        };
        if !seen.insert(digest) {
            continue;
        }
        let skill = IndexedSkill::new(skill, digest);
        // A skill that produces no terms cannot be ranked, and the reference
        // index excludes empty documents from its corpus statistics.
        if !skill.tokens.is_empty() {
            indexed.push(skill);
        }
    }
    indexed
}

/// Computes the canonical `i64` BM25 score of every document in `docs`.
///
/// The arithmetic order intentionally mirrors a straightforward scalar Okapi
/// BM25 implementation so that scores are stable across platforms.
fn score(query: &str, docs: &[&IndexedSkill]) -> Vec<i64> {
    let mut scores = vec![0i64; docs.len()];
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() || docs.is_empty() {
        return scores;
    }

    let doc_count = docs.len() as f64;
    let total_tokens: usize = docs.iter().map(|doc| doc.len()).sum();
    let avg_len = total_tokens as f64 / doc_count;
    if avg_len <= f64::EPSILON {
        return scores;
    }

    let inv_avg_len = 1.0 / avg_len;
    let k1_plus_one = K1 + 1.0;
    let k1_one_minus_b = K1 * (1.0 - B);
    let k1_b_over_avg_len = K1 * B * inv_avg_len;

    let mut raw = vec![0.0f64; docs.len()];
    for term in &query_tokens {
        let doc_freq = docs
            .iter()
            .filter(|doc| doc.term_freqs.contains_key(term))
            .count();
        if doc_freq == 0 {
            continue;
        }

        let idf = (((doc_count - doc_freq as f64 + 0.5) / (doc_freq as f64 + 0.5)) + 1.0).ln();

        for (position, doc) in docs.iter().enumerate() {
            let Some(&term_freq) = doc.term_freqs.get(term) else {
                continue;
            };
            let term_freq = term_freq as f64;
            let doc_len = doc.len() as f64;
            let denominator = term_freq + k1_one_minus_b + k1_b_over_avg_len * doc_len;
            raw[position] += idf * ((term_freq * k1_plus_one) / denominator);
        }
    }

    for (position, value) in raw.into_iter().enumerate() {
        if value > 0.0 {
            scores[position] = DeterministicScore::from_f64(value).to_raw();
        }
    }
    scores
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::test_support::{load_doc_coauthoring, load_internal_comms};

    fn fixtures() -> Vec<Skill> {
        vec![load_doc_coauthoring(), load_internal_comms()]
    }

    fn find_skill<'a>(skills: &'a [Skill], match_: &SkillMatch) -> &'a Skill {
        skills
            .iter()
            .find(|skill| skill.digest().unwrap() == match_.skill_digest)
            .expect("matched skill must be part of the corpus")
    }

    #[lyquid_test::test]
    fn ranks_the_relevant_skill_first() {
        let skills = fixtures();
        let doc_digest = skills[0].digest().unwrap();

        let matches = search("write a design doc", 2, &skills);

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].skill_digest, doc_digest);
        assert!(matches.iter().all(|match_| match_.bm25_score >= 0));
    }

    #[lyquid_test::test]
    fn respects_top_k() {
        let skills = fixtures();
        let comms_digest = skills[1].digest().unwrap();

        let matches = search("internal company newsletter update", 1, &skills);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].skill_digest, comms_digest);
    }

    #[lyquid_test::test]
    fn empty_inputs_return_no_matches() {
        let skills = fixtures();

        assert!(search("", 5, &skills).is_empty());
        assert!(search("design doc", 0, &skills).is_empty());
        assert!(search("design doc", 5, &[]).is_empty());
    }

    #[lyquid_test::test]
    fn is_deterministic_across_calls() {
        let skills = fixtures();

        let first = search("write a design doc", 2, &skills);
        let second = search("write a design doc", 2, &skills);

        assert_eq!(first, second);
    }

    #[lyquid_test::test]
    fn deduplicates_skills_by_digest() {
        let doc = load_doc_coauthoring();
        let skills = vec![doc.clone(), doc, load_internal_comms()];

        let matches = search("write a design doc", 3, &skills);
        let mut digests: Vec<_> = matches.iter().map(|match_| match_.skill_digest).collect();
        let total = digests.len();
        digests.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        digests.dedup();

        assert_eq!(digests.len(), total);
    }

    #[lyquid_test::test]
    fn canonical_scores_depend_only_on_the_selected_subset() {
        let skills = fixtures();
        let query = "write a design doc";

        let matches = search(query, 2, &skills);
        let subset: Vec<Skill> = matches
            .iter()
            .map(|match_| find_skill(&skills, match_).clone())
            .collect();

        // Searching the result subset must reproduce the exact same scores:
        // the canonical corpus is the top-k result set, not the full library.
        let rescored = search(query, subset.len(), &subset);

        assert_eq!(rescored, matches);
    }

    #[lyquid_test::test]
    fn caps_results_at_max_matches() {
        let skills: Vec<Skill> = (0..=MAX_MATCHES)
            .map(|index| {
                Skill::new(
                    format!("skill-{index}"),
                    format!("skill-{index}"),
                    "needle",
                    "",
                    Vec::new(),
                )
            })
            .collect();

        let matches = search("needle", usize::MAX, &skills);

        assert_eq!(matches.len(), MAX_MATCHES);
    }

    #[lyquid_test::test]
    fn scores_are_positive_and_ordered() {
        let matches = search("write a design doc", 2, &fixtures());

        assert!(matches.iter().all(|match_| match_.bm25_score > 0));
        assert!(
            matches
                .windows(2)
                .all(|pair| pair[0].bm25_score >= pair[1].bm25_score)
        );
    }
}
