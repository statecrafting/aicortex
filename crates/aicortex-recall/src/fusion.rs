//! Reciprocal rank fusion (spec 018 B-3, B-4, B-8).
//!
//! A memory's score is the weighted sum over channels of
//! `1 / (rank_constant + rank)`, times the importance prior. Only ranks
//! enter, so the four channels' incomparable scales never meet. Ties break
//! on memory id, and terms sum in [`Channel::ALL`] order, so the result is
//! identical on every run.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use aicortex_types::MemoryId;

use crate::channels::ChannelOutput;
use crate::query::{Channel, RecallConfig, Weights};
use crate::trace::{ByteRange, Contribution, FusedEntry, RecallTrace};

/// The prior for decayed importance `x` (B-3): `1 - span/2 + span * x`.
#[must_use]
pub fn prior(x: f64, span: f64) -> f64 {
    span.mul_add(x.clamp(0.0, 1.0), 1.0 - span / 2.0)
}

/// `weight / (rank_constant + rank)`.
#[must_use]
pub fn term(weight: f64, rank_constant: u32, rank: u32) -> f64 {
    weight / (f64::from(rank_constant) + f64::from(rank))
}

fn order(a: &(MemoryId, f64), b: &(MemoryId, f64)) -> Ordering {
    b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))
}

/// Fuse `outputs` into the best `k` entries, ranked from 1.
///
/// `priors` maps a memory to its importance prior; a memory without one
/// takes the neutral `1.0`.
#[must_use]
pub fn fuse(
    outputs: &[ChannelOutput],
    priors: &BTreeMap<MemoryId, f64>,
    config: &RecallConfig,
    k: usize,
) -> Vec<FusedEntry> {
    let mut terms: BTreeMap<MemoryId, Vec<Contribution>> = BTreeMap::new();
    let mut ranges: BTreeMap<(MemoryId, Channel), ByteRange> = BTreeMap::new();
    for channel in Channel::ALL {
        let Some(output) = outputs.iter().find(|output| output.channel == channel) else {
            continue;
        };
        let weight = config.weights.of(channel);
        for (position, candidate) in output.candidates.iter().enumerate() {
            let rank = u32::try_from(position + 1).unwrap_or(u32::MAX);
            terms
                .entry(candidate.memory_id)
                .or_default()
                .push(Contribution {
                    channel,
                    rank,
                    weight,
                    term: term(weight, config.rank_constant, rank),
                });
            if let Some(range) = candidate.range {
                ranges.insert((candidate.memory_id, channel), range);
            }
        }
    }
    let mut scored: Vec<(MemoryId, f64)> = terms
        .iter()
        .map(|(id, contributions)| {
            let sum: f64 = contributions.iter().map(|c| c.term).sum();
            (*id, sum * priors.get(id).copied().unwrap_or(1.0))
        })
        .collect();
    scored.sort_by(order);
    scored.truncate(k);
    scored
        .into_iter()
        .enumerate()
        .filter_map(|(position, (id, score))| {
            let contributions = terms.remove(&id)?;
            let strongest = contributions
                .iter()
                .max_by(|a, b| a.term.total_cmp(&b.term).then(b.channel.cmp(&a.channel)))?;
            let reason = format!("{}#{}", strongest.channel.label(), strongest.rank);
            // The winning chunk comes from the strongest chunk channel.
            let chunk = contributions
                .iter()
                .filter(|c| matches!(c.channel, Channel::Vector | Channel::Text))
                .max_by(|a, b| a.term.total_cmp(&b.term).then(b.channel.cmp(&a.channel)))
                .and_then(|c| ranges.get(&(id, c.channel)).copied());
            Some(FusedEntry {
                memory_id: id,
                rank: u32::try_from(position + 1).unwrap_or(u32::MAX),
                score,
                prior: priors.get(&id).copied().unwrap_or(1.0),
                chunk,
                reason,
                contributions,
            })
        })
        .collect()
}

/// Recompute the returned order from a trace's per-channel ranks, weights,
/// constant, and recorded priors alone (FR-005). Needs an explained trace.
#[must_use]
pub fn recompute_order(trace: &RecallTrace) -> Vec<MemoryId> {
    let weights: &Weights = &trace.weights;
    let priors = &trace.priors;
    let mut sums: BTreeMap<MemoryId, f64> = BTreeMap::new();
    for channel in Channel::ALL {
        let Some(traced) = trace.per_channel.iter().find(|t| t.channel == channel) else {
            continue;
        };
        for candidate in &traced.candidates {
            *sums.entry(candidate.memory_id).or_insert(0.0) +=
                term(weights.of(channel), trace.rank_constant, candidate.rank);
        }
    }
    let mut scored: Vec<(MemoryId, f64)> = sums
        .into_iter()
        .map(|(id, sum)| (id, sum * priors.get(&id).copied().unwrap_or(1.0)))
        .collect();
    scored.sort_by(order);
    scored.truncate(trace.fused.len());
    scored.into_iter().map(|(id, _)| id).collect()
}
