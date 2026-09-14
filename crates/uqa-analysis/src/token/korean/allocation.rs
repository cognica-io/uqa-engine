//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer native morphology leases while replacing UTF-16 term and token buffers.

use uqa_core::memory::Budgeted;

use crate::nori::{NoriOutput, NoriToken};
use crate::token::allocation::TokenBuffer;
use crate::{AnalysisError, AnalysisResult, AnalysisToken, AnalyzedText, FilteredText, TokenTerm};

impl AnalyzedText {
    pub(crate) fn from_nori_budgeted(
        output: Budgeted<NoriOutput>,
        input: &FilteredText<'_>,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let (output, memory) = output.into_parts();
        let mut buffer = TokenBuffer::new(memory.budget());
        buffer.memory.absorb(memory);
        let NoriOutput {
            tokens,
            terminal,
            final_offset_utf16,
            final_position_increment,
        } = output;
        poll()?;
        let length = input.filtered_utf16(0..input.as_str().len())?.end;
        if final_offset_utf16 != length {
            return Err(AnalysisError::MismatchedAnalysisInput {
                expected_utf16: final_offset_utf16,
                actual_utf16: length,
            });
        }
        let original_buffer_bytes = tokens.capacity() * std::mem::size_of::<NoriToken>();
        let mut pending = tokens.into_iter();
        for token in pending.by_ref() {
            poll()?;
            let token = buffer.convert_nori(token, input, poll)?;
            buffer.tokens.push(token)?;
        }
        drop(pending);
        drop(buffer.memory.split(original_buffer_bytes));
        if let Some(terminal) = terminal {
            let token = {
                let allocation = terminal;
                *allocation
            };
            drop(buffer.memory.split(std::mem::size_of::<NoriToken>()));
            let token = buffer.convert_nori(token, input, poll)?;
            buffer.set_terminal(token)?;
        }
        buffer.finish(input, final_position_increment, poll)
    }
}

impl TokenBuffer {
    fn convert_nori(
        &mut self,
        mut token: NoriToken,
        input: &FilteredText<'_>,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<AnalysisToken> {
        let length = token.term_utf16.len();
        let units = std::mem::take(&mut token.term_utf16);
        let memory = self
            .memory
            .split(units.capacity() * std::mem::size_of::<u16>());
        let term = TokenTerm::from_utf16_budgeted(Budgeted::new(units, memory), &mut *poll)?;
        let (term, memory) = term.into_parts();
        self.memory.absorb(memory);
        AnalysisToken::from_nori_term(token, term, length, input)
    }
}
