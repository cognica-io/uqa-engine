//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source predicates retain their ordered namespace, vector and scalar payload owners.

use crate::{binding::production::BindingMap, plan::OwnedScalarExpr, SQLError, ScalarExpr};
use uqa_core::memory::{BudgetedVec, MemoryReservation, ProductionControl};

enum Terms {
    Ordinary(Vec<ScalarExpr>),
    Controlled(BudgetedVec<ScalarExpr>),
}
impl Terms {
    fn as_slice(&self) -> &[ScalarExpr] {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }
}
struct Predicates {
    terms: Terms,
    // The vector and its expressions retire before their scalar payload reservation.
    memory: Option<MemoryReservation>,
}
impl Predicates {
    fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            terms: control.budget().map_or_else(
                || Terms::Ordinary(Vec::new()),
                |budget| Terms::Controlled(BudgetedVec::new(budget)),
            ),
            memory: control.empty_reservation(),
        }
    }
    fn push(
        &mut self,
        predicate: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.memory = control.combine(self.memory.take(), control.empty_reservation());
        let predicate = predicate.finish(control)?;
        match &mut self.terms {
            Terms::Ordinary(values) => values
                .try_reserve(1)
                .map_err(uqa_core::memory::MemoryError::from)?,
            Terms::Controlled(values) => values.reserve(1)?,
        }
        let (predicate, memory) = predicate.into_parts();
        match &mut self.terms {
            Terms::Ordinary(values) => values.push(predicate),
            Terms::Controlled(values) => values.push(predicate)?,
        }
        self.memory = control.combine(self.memory.take(), memory);
        Ok(())
    }
}

#[derive(Default)]
pub struct QualifierFilters {
    entries: BindingMap<Predicates>,
}
impl QualifierFilters {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn get(&self, qualifier: &str) -> Option<&[ScalarExpr]> {
        self.entries
            .get(qualifier)
            .map(|entry| entry.terms.as_slice())
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&String, &[ScalarExpr])> {
        self.entries
            .iter()
            .map(|(name, entry)| (name, entry.terms.as_slice()))
    }
    pub fn push_owned(
        &mut self,
        qualifier: &str,
        predicate: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if let Some(entry) = self.entries.get_mut(qualifier) {
            return entry.push(predicate, control);
        }
        let mut entry = Predicates::new(control);
        entry.push(predicate, control)?;
        self.entries
            .insert_with_control(qualifier, entry, control)?;
        Ok(())
    }
    pub fn push(&mut self, qualifier: &str, predicate: ScalarExpr) {
        self.push_owned(
            qualifier,
            OwnedScalarExpr::uncontrolled(predicate),
            &ProductionControl::uncontrolled(),
        )
        .expect("ordinary source predicate");
    }
    pub fn copy_with_control(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut output = Self::new();
        for (qualifier, predicates) in self.iter() {
            let mut entry = Predicates::new(control);
            for predicate in predicates {
                entry.push(predicate.copy_for_rewrite(control)?, control)?;
            }
            output
                .entries
                .insert_with_control(qualifier, entry, control)?;
        }
        Ok(output)
    }
}
impl Clone for QualifierFilters {
    fn clone(&self) -> Self {
        self.copy_with_control(&ProductionControl::uncontrolled())
            .expect("ordinary source predicates")
    }
}
impl std::fmt::Debug for QualifierFilters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
impl PartialEq for QualifierFilters {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl std::ops::Index<&str> for QualifierFilters {
    type Output = [ScalarExpr];
    fn index(&self, name: &str) -> &Self::Output {
        self.get(name).expect("source qualifier")
    }
}
impl<const N: usize> From<[(String, Vec<ScalarExpr>); N]> for QualifierFilters {
    fn from(entries: [(String, Vec<ScalarExpr>); N]) -> Self {
        entries.into_iter().collect()
    }
}
impl FromIterator<(String, Vec<ScalarExpr>)> for QualifierFilters {
    fn from_iter<I: IntoIterator<Item = (String, Vec<ScalarExpr>)>>(entries: I) -> Self {
        let mut output = Self::new();
        for (name, terms) in entries {
            output.entries.insert(
                name,
                Predicates {
                    terms: Terms::Ordinary(terms),
                    memory: None,
                },
            );
        }
        output
    }
}

#[cfg(test)]
mod tests;
