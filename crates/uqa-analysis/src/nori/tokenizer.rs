//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Korean morphology over the reference UTF-16 coordinate space.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::error::{check_limit, invalid};
use super::{DictionaryError, NoriDictionary, POSTag, POSType, UserDictionary};
use crate::AnalysisResult;

mod emission;
mod lattice;
mod viterbi;
mod word;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecompoundMode {
    None,
    #[default]
    Discard,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoriOptions {
    pub decompound_mode: DecompoundMode,
    pub output_unknown_unigrams: bool,
    pub discard_punctuation: bool,
}

impl Default for NoriOptions {
    fn default() -> Self {
        Self {
            decompound_mode: DecompoundMode::Discard,
            output_unknown_unigrams: false,
            discard_punctuation: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NoriLimits {
    pub max_input_utf16: usize,
    pub max_lattice_positions: usize,
    pub max_lattice_candidates: usize,
    pub max_tokens: usize,
    /// Bound token attributes, retained terminal attributes, and intermediate numeric units.
    pub max_output_utf16: usize,
}

impl Default for NoriLimits {
    fn default() -> Self {
        Self {
            max_input_utf16: 16 * 1024 * 1024,
            max_lattice_positions: 128 * 1024,
            max_lattice_candidates: 1_000_000,
            max_tokens: 4_000_000,
            max_output_utf16: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NoriOrigin {
    Known,
    Unknown,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoriMorpheme {
    pub surface_utf16: Vec<u16>,
    pub pos: POSTag,
}

/// Exact reference token units. User segmentation may split a surrogate pair, so UTF-16 is retained losslessly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoriToken {
    pub term_utf16: Vec<u16>,
    pub start_utf16: usize,
    pub end_utf16: usize,
    pub position_increment: u32,
    pub position_length: u32,
    pub keyword: bool,
    pub pos_type: POSType,
    pub left_pos: POSTag,
    pub right_pos: POSTag,
    pub reading: Option<String>,
    pub morphemes: Option<Vec<NoriMorpheme>>,
    pub origin: NoriOrigin,
}

/// Tokens and stream end, retaining opaque exhaustion attributes for subsequent filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoriOutput {
    pub tokens: Vec<NoriToken>,
    pub final_offset_utf16: usize,
    pub final_position_increment: u32,
    #[serde(skip)]
    pub(super) terminal: Option<Box<NoriToken>>,
}

impl NoriOutput {
    /// Materialize a source that leaves shared token attributes unchanged when exhausted.
    pub fn from_tokens(
        tokens: Vec<NoriToken>,
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

/// Immutable configuration and models; every call owns its lattice and pending tokens.
#[derive(Debug, Clone)]
pub struct KoreanTokenizer {
    model: Arc<NoriDictionary>,
    user: Option<Arc<UserDictionary>>,
    options: NoriOptions,
}

impl KoreanTokenizer {
    pub fn new(
        model: Arc<NoriDictionary>,
        user: Option<Arc<UserDictionary>>,
        options: NoriOptions,
    ) -> AnalysisResult<Self> {
        if user
            .as_ref()
            .is_some_and(|user| user.model_id() != model.id())
        {
            return Err(invalid(
                "Nori tokenizer",
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

    pub fn tokenize(&self, input: &str) -> AnalysisResult<NoriOutput> {
        self.tokenize_controlled(input, NoriLimits::default(), &mut || Ok(()))
    }

    pub fn tokenize_controlled(
        &self,
        input: &str,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<NoriOutput> {
        let units = encode_input(input, limits, poll)?;
        self.tokenize_utf16(&units, limits, poll)
    }

    /// Analyze code units directly, preserving even non-scalar reference terms and morphemes.
    pub fn tokenize_utf16(
        &self,
        input: &[u16],
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<NoriOutput> {
        poll()?;
        check_limit(
            "Nori input UTF-16 units",
            input.len(),
            limits.max_input_utf16,
        )?;
        viterbi::analyze(
            input,
            &self.model,
            self.user.as_deref(),
            self.options,
            limits,
            poll,
        )
    }
}

pub(super) fn encode_input(
    input: &str,
    limits: NoriLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Vec<u16>> {
    let mut units = Vec::new();
    for (index, unit) in input.encode_utf16().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        check_limit("Nori input UTF-16 units", index + 1, limits.max_input_utf16)?;
        units.try_reserve(1).map_err(DictionaryError::from)?;
        units.push(unit);
    }
    Ok(units)
}
