//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Own the new predicate storage while the final join pass moves existing source trees.

use super::ScalarExpr;
use uqa_core::memory::{
    BudgetedMap, BudgetedVec, MemoryError, MemoryReservation, PreparedMapEntry, Produced,
    ProductionControl,
};
use uqa_sql::SQLError;

pub(super) struct Output {
    predicates: Option<MemoryReservation>,
    conjunctions: Option<BudgetedMap<usize, MemoryReservation>>,
}

/// Before source mutation, both the output buffer and its temporary lifetime record have been admitted. Field order frees the buffer before either reservation on failure.
pub(super) struct Conjunction {
    values: Vec<ScalarExpr>,
    memory: Option<MemoryReservation>,
    entry: Option<PreparedMapEntry<usize, MemoryReservation>>,
}

impl Output {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            predicates: control.empty_reservation(),
            conjunctions: control.budget().map(BudgetedMap::new),
        }
    }

    /// Inferred equalities are installed at their query's root rewrite, after its child rewrites have completed. They are moved intact and remain in that executable until its owner releases them.
    pub(super) fn retain_predicate(&mut self, predicate: Produced<ScalarExpr>) -> ScalarExpr {
        let (value, memory) = predicate.into_parts();
        match (&mut self.predicates, memory) {
            (Some(output), Some(memory)) => output.absorb(memory),
            (None, None) => {}
            _ => unreachable!("predicate production uses the rewrite allowance"),
        }
        value
    }

    pub(super) fn prepare_conjunction(
        &self,
        count: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Conjunction, SQLError> {
        assert!(count > 1, "only multiple predicates require a conjunction");
        control.check()?;
        let mut output = if let Some(budget) = control.budget() {
            let mut values = BudgetedVec::new(budget);
            values.reserve(count)?;
            let (values, memory) = values.into_parts();
            Conjunction {
                values,
                memory: Some(memory),
                entry: None,
            }
        } else {
            let mut values = Vec::new();
            values.try_reserve_exact(count).map_err(MemoryError::from)?;
            Conjunction {
                values,
                memory: None,
                entry: None,
            }
        };
        if let Some(conjunctions) = &self.conjunctions {
            // Keep the buffer's lease in its owner until record admission has succeeded.
            output.entry = Some(conjunctions.prepare_entry(
                output.values.as_ptr() as usize,
                conjunctions.budget().empty_reservation(),
            )?);
        }
        Ok(output)
    }

    /// A parent's rewrite consumes a child's generated AND buffer. The caller must keep this lease until that buffer's consuming iterator has been destroyed.
    pub(super) fn take_conjunction_memory(
        &mut self,
        values: &[ScalarExpr],
    ) -> Option<MemoryReservation> {
        self.conjunctions
            .as_mut()
            .and_then(|conjunctions| conjunctions.remove(&(values.as_ptr() as usize)))
    }

    pub(super) fn finish(mut self) -> Option<MemoryReservation> {
        if let Some(conjunctions) = &mut self.conjunctions {
            while let Some(key) = conjunctions.iter().next().map(|(key, _)| *key) {
                let memory = conjunctions.remove(&key).expect("live output buffer");
                self.predicates
                    .as_mut()
                    .expect("controlled rewrite output")
                    .absorb(memory);
            }
        }
        self.predicates
    }
}

impl Conjunction {
    pub(super) fn push(&mut self, expression: ScalarExpr) {
        assert!(
            self.values.len() < self.values.capacity(),
            "output slots were admitted before source mutation"
        );
        self.values.push(expression);
    }

    /// Publication has no allocation or fallible operation. The output record owns only this buffer; inferred scalar payloads have their separate constructor reservations.
    pub(super) fn publish(self, output: &mut Output) -> ScalarExpr {
        let Self {
            values,
            memory,
            entry,
        } = self;
        if let Some(entry) = entry {
            let conjunctions = output
                .conjunctions
                .as_mut()
                .expect("controlled output record");
            let key = values.as_ptr() as usize;
            assert!(
                conjunctions.insert_prepared(entry).is_none(),
                "new output buffer has a unique live address"
            );
            *conjunctions.get_mut(&key).expect("published output record") =
                memory.expect("admitted output buffer");
        } else {
            assert!(memory.is_none(), "ordinary output has no reservation");
        }
        ScalarExpr::And(values)
    }
}
