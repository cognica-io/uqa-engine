//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese morphology over lossless UTF-16 coordinates and the shared rolling search.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uqa_core::memory::{Budgeted, MemoryBudget};

use super::error::{check_limit, invalid};
use super::{KuromojiDictionary, UserDictionary};
use crate::morphology::lattice::LatticeConfig;
use crate::{AnalysisError, AnalysisResult};

mod emission;
mod nbest;
mod resegment;
mod viterbi;
mod word;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KuromojiMode {
    Normal,
    #[default]
    Search,
    Extended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KuromojiOptions {
    pub mode: KuromojiMode,
    pub discard_punctuation: bool,
    pub discard_compound_token: bool,
    /// Signed extra path cost; non-positive values disable the alternative lattice.
    pub n_best_cost: i32,
}

impl Default for KuromojiOptions {
    fn default() -> Self {
        Self {
            mode: KuromojiMode::Search,
            discard_punctuation: true,
            discard_compound_token: true,
            n_best_cost: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KuromojiLimits {
    pub max_input_utf16: usize,
    pub max_lattice_positions: usize,
    pub max_lattice_candidates: usize,
    /// Cumulative emission candidates, including alternatives before span deduplication.
    pub max_tokens: usize,
    pub max_output_utf16: usize,
    pub max_resegmentation_arcs: usize,
    pub max_resegmentation_work: usize,
    /// Alternative nodes per fragment, including its boundary nodes.
    pub max_n_best_nodes: usize,
    /// Additional graph/probe work per call, shared by all examples during preparation.
    pub max_n_best_work: usize,
    /// Nonempty slash-separated examples in one preparation call.
    pub max_n_best_examples: usize,
    /// Maximum stages per chain or entries per prepared lookup set.
    pub max_filter_entries: usize,
    /// Maximum UTF-16 units in each prepared lookup set.
    pub max_filter_utf16: usize,
    /// Cumulative completion lookup, counting and emission work per call.
    pub max_completion_work: usize,
}

impl Default for KuromojiLimits {
    fn default() -> Self {
        Self {
            max_input_utf16: 16 * 1024 * 1024,
            max_lattice_positions: 128 * 1024,
            max_lattice_candidates: 1_000_000,
            max_tokens: 4_000_000,
            max_output_utf16: 64 * 1024 * 1024,
            max_resegmentation_arcs: 1_000_000,
            max_resegmentation_work: 16_000_000,
            max_n_best_nodes: 1_000_000,
            max_n_best_work: 16_000_000,
            max_n_best_examples: 1024,
            max_filter_entries: 65536,
            max_filter_utf16: 16 * 1024 * 1024,
            max_completion_work: 16_000_000,
        }
    }
}

impl LatticeConfig for KuromojiLimits {
    fn check_positions(self, required: usize) -> AnalysisResult<()> {
        check_limit(
            "Kuromoji lattice positions",
            required,
            self.max_lattice_positions,
        )?;
        Ok(())
    }

    fn check_candidates(self, required: usize) -> AnalysisResult<()> {
        check_limit(
            "Kuromoji lattice candidates",
            required,
            self.max_lattice_candidates,
        )?;
        Ok(())
    }

    fn invalid(self, reason: &'static str) -> AnalysisError {
        invalid("Kuromoji lattice", reason).into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KuromojiOrigin {
    Known,
    Unknown,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KuromojiToken {
    pub term_utf16: Vec<u16>,
    pub start_utf16: usize,
    pub end_utf16: usize,
    pub position_increment: u32,
    pub position_length: u32,
    pub keyword: bool,
    pub part_of_speech: Option<String>,
    pub base_form: Option<String>,
    pub reading: Option<String>,
    pub pronunciation: Option<String>,
    pub inflection_type: Option<String>,
    pub inflection_form: Option<String>,
    pub origin: KuromojiOrigin,
    #[serde(skip)]
    pub(crate) errors: super::AttributeErrors,
}

impl KuromojiToken {
    /// Construct a token without dictionary attributes; public fields may then be populated explicitly.
    pub fn new(term_utf16: Vec<u16>, span: std::ops::Range<usize>, origin: KuromojiOrigin) -> Self {
        Self {
            term_utf16,
            start_utf16: span.start,
            end_utf16: span.end,
            position_increment: 1,
            position_length: 1,
            keyword: false,
            part_of_speech: None,
            base_form: None,
            reading: None,
            pronunciation: None,
            inflection_type: None,
            inflection_form: None,
            origin,
            errors: crate::kuromoji::AttributeErrors::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KuromojiOutput {
    pub tokens: Vec<KuromojiToken>,
    pub final_offset_utf16: usize,
    pub final_position_increment: u32,
    #[serde(skip)]
    pub(crate) terminal: Option<Box<KuromojiToken>>,
}

impl KuromojiOutput {
    pub(crate) fn validate_attributes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        for token in &self.tokens {
            poll()?;
            token.errors.validate()?;
        }
        Ok(())
    }

    /// Materialize a source that leaves token attributes unchanged when exhausted.
    pub fn from_tokens(
        tokens: Vec<KuromojiToken>,
        final_offset_utf16: usize,
        final_position_increment: u32,
    ) -> Self {
        Self {
            tokens,
            final_offset_utf16,
            final_position_increment,
            terminal: None,
        }
    }
}

/// Immutable models and configuration; each call owns its lattice, resegmentation and output.
///
/// ```
/// use uqa_analysis::kuromoji::{JapaneseTokenizer, KuromojiOptions, KuromojiResources};
/// let dictionary = KuromojiResources::default().load_default()?;
/// let tokenizer = JapaneseTokenizer::new(dictionary.model().clone(), None, KuromojiOptions::default())?;
/// let output = tokenizer.tokenize("関西国際空港")?;
/// let terms: Vec<_> = output.tokens.iter().map(|token| String::from_utf16(&token.term_utf16).unwrap()).collect();
/// assert_eq!(terms, ["関西", "国際", "空港"]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct JapaneseTokenizer {
    model: Arc<KuromojiDictionary>,
    user: Option<Arc<UserDictionary>>,
    options: KuromojiOptions,
}

impl JapaneseTokenizer {
    pub fn new(
        model: Arc<KuromojiDictionary>,
        user: Option<Arc<UserDictionary>>,
        options: KuromojiOptions,
    ) -> AnalysisResult<Self> {
        if user
            .as_ref()
            .is_some_and(|user| user.model_id() != model.id())
        {
            return Err(invalid(
                "Kuromoji tokenizer",
                "user rules were compiled against another model",
            )
            .into());
        }
        Ok(Self {
            model,
            user,
            options,
        })
    }

    pub fn tokenize(&self, input: &str) -> AnalysisResult<KuromojiOutput> {
        self.tokenize_controlled(input, KuromojiLimits::default(), &mut || Ok(()))
    }

    pub fn tokenize_controlled(
        &self,
        input: &str,
        limits: KuromojiLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<KuromojiOutput> {
        let budget = MemoryBudget::new(usize::MAX);
        Ok(self
            .tokenize_budgeted(input, limits, &budget, poll)?
            .into_parts()
            .0)
    }

    pub fn tokenize_budgeted(
        &self,
        input: &str,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<KuromojiOutput>> {
        self.tokenize_text_budgeted(input, limits, budget, poll, false)
    }

    pub(crate) fn tokenize_for_filters_budgeted(
        &self,
        input: &str,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<KuromojiOutput>> {
        self.tokenize_text_budgeted(input, limits, budget, poll, true)
    }

    fn tokenize_text_budgeted(
        &self,
        input: &str,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
        deferred: bool,
    ) -> AnalysisResult<Budgeted<KuromojiOutput>> {
        poll()?;
        let units = crate::morphology::input::encode(input, budget, poll, |length| {
            check_limit(
                "Kuromoji input UTF-16 units",
                length,
                limits.max_input_utf16,
            )
            .map_err(Into::into)
        })?;
        if deferred {
            viterbi::analyze_filtering(&units, self, limits, budget, poll)
        } else {
            self.tokenize_utf16_budgeted(&units, limits, budget, poll)
        }
    }

    pub fn tokenize_utf16(
        &self,
        input: &[u16],
        limits: KuromojiLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<KuromojiOutput> {
        let budget = MemoryBudget::new(usize::MAX);
        Ok(self
            .tokenize_utf16_budgeted(input, limits, &budget, poll)?
            .into_parts()
            .0)
    }

    /// Borrowed input remains caller-owned; every tokenizer allocation shares the retained allowance.
    pub fn tokenize_utf16_budgeted(
        &self,
        input: &[u16],
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<KuromojiOutput>> {
        poll()?;
        check_limit(
            "Kuromoji input UTF-16 units",
            input.len(),
            limits.max_input_utf16,
        )?;
        viterbi::analyze(
            input,
            &self.model,
            self.user.as_deref(),
            self.options,
            limits,
            budget,
            poll,
        )
    }
}

#[cfg(test)]
pub(super) mod tests;
