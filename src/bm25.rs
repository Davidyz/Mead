//! Deterministic BM25 search over the locally available skills.
//!
//! The [`khive_bm25`] index itself is not usable in the wasm guest because it
//! records timings with [`std::time::Instant`], which is unimplemented on
//! `wasm32-unknown-unknown`. We therefore reuse its tokenizer and fixed-point
//! score type but compute the BM25 sum with explicit scalar `f64` arithmetic.
//! Keeping the evaluation order fixed makes the scores reproducible across
//! peers and platforms.

// TODO: use khive_bm25::Bm25Index once its Instant-based metrics can be disabled (or vendor the tokenizer and score types) so this scorer cannot drift from upstream.

use std::cell::RefCell;
use std::rc::Rc;

use khive_bm25::{DeterministicScore, tokenize};
use lyquid::prelude::{Hash, HashMap, new_hashmap, new_hashset};

use crate::skill::Skill;
use crate::types::{Bm25Score, MAX_MATCHES, SkillMatch};

/// Term saturation parameter.
const K1: f64 = 1.2;
/// Length normalization parameter.
const B: f64 = 0.75;

thread_local! {
    /// Memoizes indexed skills by digest for the lifetime of the guest.
    /// A skill's digest covers its canonical encoding, so a cache hit is
    /// equivalent to re-tokenizing the same skill.
    static INDEX_CACHE: RefCell<IndexCache> = RefCell::new(IndexCache::new(1024));
}

/// A bounded, least-recently-used cache of indexed skills.
struct IndexCache {
    entries: HashMap<Hash, CacheEntry>,
    /// Most recently used digest.
    head: Option<Hash>,
    /// Least recently used digest.
    tail: Option<Hash>,
    /// Maximum number of entries kept before the least-recently-used one is
    /// evicted. Zero disables caching.
    max_cached: usize,
}

struct CacheEntry {
    skill: IndexedSkill,
    prev: Option<Hash>,
    next: Option<Hash>,
}

impl IndexCache {
    fn new(max_cached: usize) -> Self {
        Self {
            entries: new_hashmap(),
            head: None,
            tail: None,
            max_cached,
        }
    }

    fn get(&mut self, digest: &Hash) -> Option<IndexedSkill> {
        let cached = self.entries.get(digest)?.skill.clone();
        self.touch(digest);
        Some(cached)
    }

    fn insert(&mut self, digest: Hash, skill: IndexedSkill) {
        if self.max_cached == 0 {
            return;
        }

        if self.entries.contains_key(&digest) {
            if let Some(entry) = self.entries.get_mut(&digest) {
                entry.skill = skill;
            }
            self.touch(&digest);
            return;
        }

        while self.entries.len() >= self.max_cached {
            if !self.evict_least_recently_used() {
                break;
            }
        }

        self.entries.insert(
            digest,
            CacheEntry {
                skill,
                prev: None,
                next: None,
            },
        );
        self.push_front(digest);
    }

    #[cfg(test)]
    fn remove(&mut self, digest: &Hash) -> Option<IndexedSkill> {
        if !self.entries.contains_key(digest) {
            return None;
        }
        self.detach(digest);
        self.entries.remove(digest).map(|entry| entry.skill)
    }

    #[cfg(test)]
    fn clear(&mut self) {
        self.entries.clear();
        self.head = None;
        self.tail = None;
    }

    /// Marks `digest` as the most recently used entry.
    fn touch(&mut self, digest: &Hash) {
        if self.head == Some(*digest) {
            return;
        }
        self.detach(digest);
        self.push_front(*digest);
    }

    /// Removes `digest` from the recency list without removing its entry.
    fn detach(&mut self, digest: &Hash) {
        let Some((prev, next)) = self
            .entries
            .get(digest)
            .map(|entry| (entry.prev, entry.next))
        else {
            return;
        };

        match prev {
            Some(prev) => {
                if let Some(entry) = self.entries.get_mut(&prev) {
                    entry.next = next;
                }
            }
            None => self.head = next,
        }
        match next {
            Some(next) => {
                if let Some(entry) = self.entries.get_mut(&next) {
                    entry.prev = prev;
                }
            }
            None => self.tail = prev,
        }

        if let Some(entry) = self.entries.get_mut(digest) {
            entry.prev = None;
            entry.next = None;
        }
    }

    /// Inserts `digest` at the most-recently-used end of the list.
    fn push_front(&mut self, digest: Hash) {
        let previous_head = self.head;
        if let Some(entry) = self.entries.get_mut(&digest) {
            entry.prev = None;
            entry.next = previous_head;
        }

        match previous_head {
            Some(head) => {
                if let Some(entry) = self.entries.get_mut(&head) {
                    entry.prev = Some(digest);
                }
            }
            None => self.tail = Some(digest),
        }
        self.head = Some(digest);
    }

    fn evict_least_recently_used(&mut self) -> bool {
        let Some(lru) = self.tail else {
            return false;
        };
        self.detach(&lru);
        self.entries.remove(&lru).is_some()
    }
}

/// A skill prepared for scoring.
///
/// Token storage is shared via [`Rc`] so cache hits are reference-count bumps
/// rather than full copies.
#[derive(Clone)]
struct IndexedSkill {
    digest: Hash,
    tokens: Rc<Vec<String>>,
    term_freqs: Rc<HashMap<String, u32>>,
}

impl IndexedSkill {
    fn new(skill: &Skill, digest: Hash) -> Self {
        if let Some(cached) = INDEX_CACHE.with(|cache| cache.borrow_mut().get(&digest)) {
            return cached;
        }

        let tokens = Rc::new(tokenize(&skill.search_text()));
        let mut term_freqs = new_hashmap();
        for token in tokens.iter() {
            *term_freqs.entry(token.clone()).or_insert(0) += 1;
        }
        let indexed = Self {
            digest,
            tokens,
            term_freqs: Rc::new(term_freqs),
        };
        // Empty documents are discarded by `index_skills`, so caching them
        // would only consume slots.
        if !indexed.tokens.is_empty() {
            INDEX_CACHE.with(|cache| {
                cache.borrow_mut().insert(digest, indexed.clone());
            });
        }
        indexed
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

/// Scores exactly `skills` as the entire corpus.
///
/// This is the corpus used by stage 2 of [`search`], so the returned scores are
/// the canonical values committed in a [`SkillMatch`].
pub fn score_corpus(query: &str, skills: &[Skill]) -> Vec<(Hash, Bm25Score)> {
    let indexed = index_skills(skills);
    let all: Vec<&IndexedSkill> = indexed.iter().collect();
    let scores = score(query, &all);
    indexed
        .into_iter()
        .zip(scores)
        .map(|(skill, score)| (skill.digest, score))
        .collect()
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

    /// Removes a test digest from the cache even if an assertion panics.
    struct CacheGuard(Hash);

    impl Drop for CacheGuard {
        fn drop(&mut self) {
            INDEX_CACHE.with(|cache| {
                cache.borrow_mut().remove(&self.0);
            });
        }
    }

    fn synthetic_skill(index: usize) -> IndexedSkill {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
        let digest = Hash::from_bytes(bytes);
        IndexedSkill {
            digest,
            tokens: Rc::new(Vec::new()),
            term_freqs: Rc::new(new_hashmap()),
        }
    }

    fn recency_order(cache: &IndexCache) -> Vec<Hash> {
        let mut forward = Vec::new();
        let mut cursor = cache.head;
        while let Some(digest) = cursor {
            assert!(
                forward.len() <= cache.entries.len(),
                "forward recency list contains a cycle"
            );
            assert!(
                !forward.contains(&digest),
                "forward recency list contains a duplicate"
            );
            forward.push(digest);
            cursor = cache.entries.get(&digest).and_then(|entry| entry.next);
        }
        assert_eq!(forward.len(), cache.entries.len());

        let mut backward = Vec::new();
        let mut cursor = cache.tail;
        while let Some(digest) = cursor {
            assert!(
                backward.len() <= cache.entries.len(),
                "backward recency list contains a cycle"
            );
            assert!(
                !backward.contains(&digest),
                "backward recency list contains a duplicate"
            );
            backward.push(digest);
            cursor = cache.entries.get(&digest).and_then(|entry| entry.prev);
        }
        assert_eq!(backward.len(), cache.entries.len());
        backward.reverse();
        assert_eq!(forward, backward);

        if let Some(head) = cache.head {
            assert_eq!(cache.entries.get(&head).and_then(|entry| entry.prev), None);
        }
        if let Some(tail) = cache.tail {
            assert_eq!(cache.entries.get(&tail).and_then(|entry| entry.next), None);
        }
        forward
    }

    /// Clears the shared cache before and after a test, even on panic.
    struct CacheReset;

    impl CacheReset {
        fn new() -> Self {
            INDEX_CACHE.with(|cache| cache.borrow_mut().clear());
            Self
        }
    }

    impl Drop for CacheReset {
        fn drop(&mut self) {
            INDEX_CACHE.with(|cache| cache.borrow_mut().clear());
        }
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
        let _reset = CacheReset::new();
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

    #[lyquid_test::test]
    fn indexing_a_miss_populates_the_cache() {
        let skill = Skill::new(
            "cache-miss-skill",
            "cache-miss-skill",
            "needle in the haystack",
            "",
            Vec::new(),
        );
        let digest = skill.digest().unwrap();
        let _guard = CacheGuard(digest);

        INDEX_CACHE.with(|cache| cache.borrow_mut().remove(&digest));
        let indexed = IndexedSkill::new(&skill, digest);

        assert!(!indexed.tokens.is_empty());
        let cached = INDEX_CACHE.with(|cache| {
            cache
                .borrow()
                .entries
                .get(&digest)
                .map(|entry| entry.skill.clone())
        });
        let cached = cached.expect("a miss must cache its result");
        assert_eq!(cached.tokens, indexed.tokens);
        assert_eq!(cached.term_freqs, indexed.term_freqs);
    }

    #[lyquid_test::test]
    fn indexing_a_hit_reads_from_the_cache() {
        let skill = Skill::new(
            "cache-hit-skill",
            "cache-hit-skill",
            "text that would tokenize differently",
            "",
            Vec::new(),
        );
        let digest = skill.digest().unwrap();
        let _guard = CacheGuard(digest);
        let mut term_freqs = new_hashmap();
        term_freqs.insert("cached-sentinel".to_string(), 1);
        let sentinel = IndexedSkill {
            digest,
            tokens: Rc::new(vec!["cached-sentinel".to_string()]),
            term_freqs: Rc::new(term_freqs),
        };
        INDEX_CACHE.with(|cache| {
            cache.borrow_mut().insert(digest, sentinel.clone());
        });

        let indexed = IndexedSkill::new(&skill, digest);

        // A hit must return the cached document verbatim, not re-tokenize.
        assert_eq!(indexed.tokens, sentinel.tokens);
        assert_eq!(indexed.term_freqs, sentinel.term_freqs);
    }

    #[lyquid_test::test]
    fn cached_search_matches_cold_search() {
        let _reset = CacheReset::new();
        let skills = fixtures();
        INDEX_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            for skill in &skills {
                cache.remove(&skill.digest().unwrap());
            }
        });

        let cold = search("write a design doc", 2, &skills);
        let warm = search("write a design doc", 2, &skills);

        assert_eq!(cold, warm);
    }

    #[lyquid_test::test]
    fn cache_evicts_the_least_recently_used_entry() {
        let max_cached = 3;
        let mut cache = IndexCache::new(max_cached);
        let entries: Vec<IndexedSkill> = (0..=max_cached).map(synthetic_skill).collect();
        for skill in &entries[..max_cached] {
            cache.insert(skill.digest, skill.clone());
        }

        // Touch the oldest entry, making the second entry the LRU one.
        assert!(cache.get(&entries[0].digest).is_some());
        cache.insert(entries[max_cached].digest, entries[max_cached].clone());

        assert!(cache.get(&entries[0].digest).is_some());
        assert!(cache.get(&entries[1].digest).is_none());
        assert_eq!(cache.entries.len(), max_cached);
        assert_eq!(recency_order(&cache).len(), max_cached);
    }

    #[lyquid_test::test]
    fn zero_capacity_disables_caching() {
        let mut cache = IndexCache::new(0);
        let skill = synthetic_skill(0);

        cache.insert(skill.digest, skill.clone());

        assert!(cache.entries.is_empty());
        assert!(cache.get(&skill.digest).is_none());
    }

    #[lyquid_test::test]
    fn repeated_indexing_reuses_the_cached_allocation() {
        let skill = Skill::new(
            "cache-natural-hit-skill",
            "cache-natural-hit-skill",
            "needle in the haystack",
            "",
            Vec::new(),
        );
        let digest = skill.digest().unwrap();
        let _guard = CacheGuard(digest);
        INDEX_CACHE.with(|cache| cache.borrow_mut().remove(&digest));

        let first = IndexedSkill::new(&skill, digest);
        let second = IndexedSkill::new(&skill, digest);

        assert!(Rc::ptr_eq(&first.tokens, &second.tokens));
        assert!(Rc::ptr_eq(&first.term_freqs, &second.term_freqs));
    }
}
