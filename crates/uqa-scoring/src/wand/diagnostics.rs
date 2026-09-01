//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Upper-bound tightness diagnostics and adaptive scorer.

use std::sync::Arc;

use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use crate::error::invalid_input;
use crate::scorer::Scorer;
use crate::ScoringResult;

/// Track upper-bound tightness: ratio of `actual_max / upper_bound` per
/// posting list, averaged across all observations.
#[derive(Debug, Default, Clone)]
pub struct BoundTightnessAnalyzer {
    pairs: Vec<(f64, f64)>,
}

impl BoundTightnessAnalyzer {
    pub fn record(&mut self, upper_bound: f64, actual_max: f64) -> ScoringResult<()> {
        if !upper_bound.is_finite() || upper_bound < 0.0 {
            return Err(invalid_input(format!(
                "upper bound must be finite and non-negative, got {upper_bound}"
            )));
        }
        if !actual_max.is_finite() || actual_max < 0.0 {
            return Err(invalid_input(format!(
                "actual maximum must be finite and non-negative, got {actual_max}"
            )));
        }
        if actual_max > upper_bound {
            return Err(invalid_input(format!(
                "actual maximum {actual_max} exceeds upper bound {upper_bound}"
            )));
        }
        self.pairs.push((upper_bound, actual_max));
        Ok(())
    }

    pub fn tightness_ratio(&self) -> f64 {
        if self.pairs.is_empty() {
            return 1.0;
        }
        let n = self.pairs.len() as f64;
        let s: f64 = self
            .pairs
            .iter()
            .map(|&(ub, am)| if ub > 0.0 { (am / ub).min(1.0) } else { 1.0 })
            .sum();
        s / n
    }

    pub fn slack(&self) -> f64 {
        1.0 - self.tightness_ratio()
    }

    pub fn worst_bound_index(&self) -> usize {
        self.pairs
            .iter()
            .enumerate()
            .min_by(|(_, (ub_a, actual_a)), (_, (ub_b, actual_b))| {
                let ratio_a = if *ub_a > 0.0 {
                    (*actual_a / *ub_a).min(1.0)
                } else {
                    1.0
                };
                let ratio_b = if *ub_b > 0.0 {
                    (*actual_b / *ub_b).min(1.0)
                } else {
                    1.0
                };
                ratio_a.total_cmp(&ratio_b)
            })
            .map_or(0, |(idx, _)| idx)
    }

    pub fn clear(&mut self) {
        self.pairs.clear();
    }
}

pub struct AdaptiveWANDScorer {
    pub scorers: Vec<Arc<dyn Scorer>>,
    pub k: usize,
    pub posting_lists: Vec<PostingList>,
    pub tightening_factor: f64,
    pub analyzer: BoundTightnessAnalyzer,
}

impl AdaptiveWANDScorer {
    pub fn new(
        scorers: Vec<Arc<dyn Scorer>>,
        k: usize,
        posting_lists: Vec<PostingList>,
        tightening_factor: f64,
    ) -> ScoringResult<Self> {
        validate_adaptive_inputs(&scorers, &posting_lists, tightening_factor)?;
        Ok(Self {
            scorers,
            k,
            posting_lists,
            tightening_factor,
            analyzer: BoundTightnessAnalyzer::default(),
        })
    }

    pub fn compute_upper_bounds(&self) -> ScoringResult<Vec<f64>> {
        validate_adaptive_inputs(&self.scorers, &self.posting_lists, self.tightening_factor)?;
        self.scorers
            .iter()
            .zip(&self.posting_lists)
            .map(|(scorer, pl)| {
                let df = u64::try_from(pl.len())
                    .map_err(|_| invalid_input("posting-list length does not fit in u64"))?;
                let bound = scorer.term_upper_bound(df) * self.tightening_factor;
                if bound.is_finite() && bound >= 0.0 {
                    Ok(bound)
                } else {
                    Err(invalid_input(format!(
                        "adaptive WAND bound must be finite and non-negative, got {bound}"
                    )))
                }
            })
            .collect()
    }

    pub fn score_top_k(&mut self) -> ScoringResult<PostingList> {
        validate_adaptive_inputs(&self.scorers, &self.posting_lists, self.tightening_factor)?;
        self.analyzer.clear();
        for (scorer, pl) in self.scorers.iter().zip(&self.posting_lists) {
            let df = u64::try_from(pl.len())
                .map_err(|_| invalid_input("posting-list length does not fit in u64"))?;
            let upper = scorer.term_upper_bound(df);
            let actual = pl
                .iter()
                .map(|entry| entry.payload.score)
                .fold(0.0_f64, f64::max);
            self.analyzer.record(upper, actual)?;
        }

        let mut scores: std::collections::BTreeMap<DocId, f64> = std::collections::BTreeMap::new();
        for pl in &self.posting_lists {
            for entry in pl {
                let score = scores.entry(entry.doc_id).or_insert(0.0);
                *score += entry.payload.score;
                if !score.is_finite() || *score < 0.0 {
                    return Err(invalid_input(format!(
                        "adaptive WAND aggregate score must be finite and non-negative, got {score}"
                    )));
                }
            }
        }
        let mut entries: Vec<PostingEntry> = scores
            .into_iter()
            .map(|(doc_id, score)| PostingEntry::new(doc_id, Payload::with_score(score)))
            .collect();
        entries.sort_by(|a, b| {
            b.payload
                .score
                .total_cmp(&a.payload.score)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        entries.truncate(self.k);
        Ok(PostingList::from_unsorted(entries))
    }
}

fn validate_adaptive_inputs(
    scorers: &[Arc<dyn Scorer>],
    posting_lists: &[PostingList],
    tightening_factor: f64,
) -> ScoringResult<()> {
    if scorers.len() != posting_lists.len() {
        return Err(invalid_input(format!(
            "adaptive WAND requires one scorer per posting list, got {} scorers and {} lists",
            scorers.len(),
            posting_lists.len()
        )));
    }
    if !tightening_factor.is_finite() || !(0.0..=1.0).contains(&tightening_factor) {
        return Err(invalid_input(format!(
            "adaptive WAND tightening factor must be finite and in [0, 1], got {tightening_factor}"
        )));
    }
    for posting_list in posting_lists {
        for entry in posting_list {
            if !entry.payload.score.is_finite() || entry.payload.score < 0.0 {
                return Err(invalid_input(format!(
                    "adaptive WAND input score must be finite and non-negative, got {} for document {}",
                    entry.payload.score, entry.doc_id
                )));
            }
        }
    }
    Ok(())
}
