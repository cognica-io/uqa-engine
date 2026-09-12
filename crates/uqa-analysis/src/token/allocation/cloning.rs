//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserved token copies preserve attributes and reuse retained source allocations.

use uqa_core::memory::{Budgeted, MemoryBudget};

use super::{AnalysisToken, AnalyzedText, TokenBuffer};
use crate::term::TermBoundary;
use crate::{AnalysisResult, SourceOffsets, TokenTerm};

impl AnalysisToken {
    /// Copy the term and morphology into independent reservations, retaining all position and source attributes.
    pub fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let term = self.term.clone_budgeted(budget, &mut poll)?;
        self.copy_attributes(term, self.verbatim, budget, &mut poll)
    }

    pub(crate) fn rewrite_budgeted(
        &self,
        term: Budgeted<TokenTerm>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let verbatim = self.verbatim && self.term.eq_with_control(&term, poll)?;
        self.copy_attributes(term, verbatim, budget, poll)
    }

    fn copy_attributes(
        &self,
        term: Budgeted<TokenTerm>,
        verbatim: bool,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        poll()?;
        #[cfg(feature = "nori")]
        let morphology = self
            .korean_morphology
            .as_ref()
            .map(|value| value.clone_budgeted(budget, poll))
            .transpose()?;
        #[cfg(not(feature = "nori"))]
        let _ = budget;
        let (term, memory) = term.into_parts();
        #[cfg(feature = "nori")]
        let (korean_morphology, memory) = match morphology {
            Some(morphology) => {
                let (morphology, allocation) = morphology.into_parts();
                let mut memory = memory;
                memory.absorb(allocation);
                (Some(morphology), memory)
            }
            None => (None, memory),
        };
        Ok(Budgeted::new(
            Self {
                term,
                offsets: self.offsets.clone(),
                position_increment: self.position_increment,
                position_length: self.position_length,
                keyword: self.keyword,
                filtered_utf16: self.filtered_utf16.clone(),
                #[cfg(feature = "nori")]
                korean_morphology,
                verbatim,
            },
            memory,
        ))
    }

    pub(crate) fn substring_budgeted(
        &self,
        start: TermBoundary,
        end: TermBoundary,
        complete: TermBoundary,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let term = self
            .term
            .substring_budgeted(start.offset..end.offset, budget, poll)?;
        let (mut token, memory) = self
            .copy_attributes(term, self.verbatim, budget, poll)?
            .into_parts();
        if self.verbatim {
            if let Some(offsets) = &self.offsets {
                token.offsets = Some(SourceOffsets {
                    utf8: offsets.utf8.start + start.offset..offsets.utf8.start + end.offset,
                    utf16: offsets.utf16.start + start.utf16..offsets.utf16.start + end.utf16,
                });
                if let Some(filtered) = &self.filtered_utf16 {
                    if filtered.len() == complete.utf16 {
                        token.filtered_utf16 =
                            Some(filtered.start + start.utf16..filtered.start + end.utf16);
                    }
                }
            }
        }
        Ok(Budgeted::new(token, memory))
    }
}

impl AnalyzedText {
    /// Copy owned token buffers into this allowance while sharing retained source allocations with the original.
    ///
    /// Every term, morphology value, token buffer and hidden terminal token receives its own reservation. Existing source projection and mapping leases remain shared with the input. Failure leaves the borrowed result unchanged.
    ///
    /// ```
    /// use uqa_analysis::Tokenizer;
    /// use uqa_core::memory::MemoryBudget;
    ///
    /// let budget = MemoryBudget::new(64 * 1024);
    /// let input = Tokenizer::Whitespace.tokenize_with_offsets_budgeted(
    ///     "韓🙂 UQA", &budget, || Ok(()),
    /// )?;
    /// let copied = input.clone_budgeted(&budget, || Ok(()))?;
    /// assert_eq!(*copied, *input);
    /// drop(input);
    /// assert!(budget.used() >= copied.reserved_bytes());
    /// drop(copied);
    /// assert_eq!(budget.used(), 0);
    /// # Ok::<(), uqa_analysis::AnalysisError>(())
    /// ```
    pub fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        poll()?;
        let mut output = TokenBuffer::new(budget);
        output.tokens.reserve(self.batch.tokens.len())?;
        for token in &self.batch.tokens {
            output.push(token.clone_budgeted(budget, &mut poll)?)?;
        }
        if let Some(terminal) = &self.batch.terminal {
            let terminal = terminal.clone_budgeted(budget, &mut poll)?;
            output.memory.grow(std::mem::size_of::<AnalysisToken>())?;
            let (terminal, memory) = terminal.into_parts();
            output.memory.absorb(memory);
            output.terminal = Some(Box::new(terminal));
        }
        output.finish_retained(
            self.final_offsets.clone(),
            self.batch.final_position_increment,
            #[cfg(feature = "nori")]
            self.projection.clone(),
            &mut poll,
        )
    }
}
