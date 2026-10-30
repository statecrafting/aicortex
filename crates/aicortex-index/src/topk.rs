//! The bounded top-k heap (spec 016 B-2, B-4).
//!
//! A min-heap of at most `k` entries whose root is the worst retained
//! candidate, so each scored row costs one comparison against the root and,
//! only when it beats the root, one replacement. Memory is `k` entries no
//! matter how many rows are offered.
//!
//! The order is total and deterministic: score descending, then chunk id
//! ascending (B-4). Scores compare with [`f64::total_cmp`], so even a value
//! that should never occur cannot make two runs disagree.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use aicortex_types::MemoryId;

/// One per-channel candidate: a chunk and its score in that channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// The chunk, as spec 015's `chunk` table names it.
    pub chunk_id: String,
    /// The memory the chunk belongs to.
    pub memory_id: MemoryId,
    /// The chunk's position within its memory.
    pub ordinal: u32,
    /// The channel's score: cosine similarity for the vector channel.
    pub score: f64,
}

/// A retained entry: the score, the chunk id the tie breaks on, and the
/// caller's payload.
#[derive(Debug)]
struct Entry<T> {
    score: f64,
    chunk_id: String,
    payload: T,
}

impl<T> Entry<T> {
    /// Greater is better: higher score, then the lexically smaller chunk id.
    fn rank(&self, other: &Self) -> Ordering {
        rank(self.score, &self.chunk_id, other.score, &other.chunk_id)
    }
}

/// `Greater` when `(a_score, a_id)` ranks above `(b_score, b_id)` (B-4).
fn rank(a_score: f64, a_id: &str, b_score: f64, b_id: &str) -> Ordering {
    a_score.total_cmp(&b_score).then_with(|| b_id.cmp(a_id))
}

impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.rank(other) == Ordering::Equal
    }
}

impl<T> Eq for Entry<T> {}

impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank(other)
    }
}

/// At most `k` best entries, each carrying a payload `T`.
#[derive(Debug)]
pub struct TopK<T> {
    k: usize,
    heap: BinaryHeap<Reverse<Entry<T>>>,
}

impl<T> TopK<T> {
    /// An empty heap retaining at most `k` entries.
    #[must_use]
    pub fn new(k: usize) -> Self {
        Self {
            k,
            heap: BinaryHeap::with_capacity(k),
        }
    }

    /// Whether an entry scored `score` with `chunk_id` would be retained.
    ///
    /// The scan asks first so a row that loses costs no move at all.
    #[must_use]
    pub fn admits(&self, score: f64, chunk_id: &str) -> bool {
        if self.k == 0 {
            return false;
        }
        if self.heap.len() < self.k {
            return true;
        }
        self.heap.peek().is_some_and(|Reverse(worst)| {
            rank(score, chunk_id, worst.score, &worst.chunk_id) == Ordering::Greater
        })
    }

    /// Offer one entry; it is kept only if it ranks among the best `k`.
    pub fn offer(&mut self, score: f64, chunk_id: String, payload: T) {
        if !self.admits(score, &chunk_id) {
            return;
        }
        if self.heap.len() >= self.k {
            self.heap.pop();
        }
        self.heap.push(Reverse(Entry {
            score,
            chunk_id,
            payload,
        }));
    }

    /// How many entries are retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether nothing is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// The retained entries, best first: `(score, chunk_id, payload)`.
    #[must_use]
    pub fn into_sorted(self) -> Vec<(f64, String, T)> {
        // `into_sorted_vec` is ascending in `Reverse<Entry>`, which is
        // descending in rank: best first.
        self.heap
            .into_sorted_vec()
            .into_iter()
            .map(|Reverse(entry)| (entry.score, entry.chunk_id, entry.payload))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_best_k_in_order_with_ties_by_chunk_id() {
        let mut top = TopK::new(3);
        for (score, id) in [
            (0.5, "c"),
            (0.9, "b"),
            (0.5, "a"),
            (0.1, "z"),
            (0.9, "a"),
            (0.5, "b"),
        ] {
            top.offer(score, id.to_owned(), ());
        }
        let ids: Vec<(f64, String)> = top
            .into_sorted()
            .into_iter()
            .map(|(score, id, ())| (score, id))
            .collect();
        assert_eq!(
            ids,
            vec![
                (0.9, "a".to_owned()),
                (0.9, "b".to_owned()),
                (0.5, "a".to_owned())
            ]
        );
    }

    #[test]
    fn a_zero_k_retains_nothing() {
        let mut top = TopK::new(0);
        top.offer(1.0, "a".to_owned(), ());
        assert!(top.is_empty());
    }
}
