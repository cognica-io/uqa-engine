//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow lexical definitions while their invocation owns admitted producer states.

use super::{CtePlan, PlanCost};
use std::cell::Cell;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity, RelationName,
};
use uqa_sql::SQLError;

#[derive(Clone, Copy)]
pub(super) enum ProducerState {
    Unestimated,
    Estimating,
    Estimated(PlanCost),
}

pub(super) struct Definitions<'p, 's> {
    ctes: &'p [CtePlan],
    names: Produced<Vec<(RelationName<'p>, usize)>>,
    producers: Produced<Vec<Cell<ProducerState>>>,
    outer: Option<Namespace<'p, 's>>,
    parent: Option<usize>,
}

#[derive(Clone, Copy)]
pub(super) struct Namespace<'p, 's> {
    definitions: &'s Definitions<'p, 's>,
    ceiling: usize,
}

#[derive(Clone, Copy)]
pub(super) struct Definition<'p, 's> {
    definitions: &'s Definitions<'p, 's>,
    index: usize,
}

impl<'p, 's> Definitions<'p, 's> {
    pub(super) fn new(
        ctes: &'p [CtePlan],
        outer: Option<Namespace<'p, 's>>,
        parent: Option<usize>,
        control: ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut names = ProductionVec::new(control);
        let mut producers = ProductionVec::new(control);
        for (index, cte) in ctes.iter().enumerate() {
            names.push_copy((RelationName::decoded(&cte.name), index))?;
            producers.push_produced(control.finish(
                Cell::new(ProducerState::Unestimated),
                control.empty_reservation(),
            )?)?;
        }
        names
            .as_mut_slice()
            .sort_unstable_by(|left, right| left.0.cmp(&right.0));
        Ok(Self {
            ctes,
            names: names.finish()?,
            producers: producers.finish()?,
            outer,
            parent,
        })
    }

    pub(super) fn namespace(&self) -> Option<Namespace<'p, '_>> {
        if self.ctes.is_empty() {
            self.outer
        } else {
            Some(Namespace {
                definitions: self,
                ceiling: self.ctes.len(),
            })
        }
    }

    pub(super) fn definition(&self, index: usize) -> Definition<'p, '_> {
        Definition {
            definitions: self,
            index,
        }
    }

    pub(super) fn states(&self) -> impl Iterator<Item = ProducerState> + '_ {
        self.producers.iter().map(Cell::get)
    }
}

impl<'p, 's> Namespace<'p, 's> {
    pub(super) fn resolve(self, reference: &str) -> Option<Definition<'p, 's>> {
        let reference = RelationIdentity::borrowed_reference(reference)?;
        if reference.schema.is_some() {
            return None;
        }
        let mut current = Some(self);
        while let Some(namespace) = current {
            if let Ok(position) = namespace
                .definitions
                .names
                .binary_search_by(|(name, _)| name.cmp(&reference.name))
            {
                let index = namespace.definitions.names[position].1;
                if index < namespace.ceiling {
                    return Some(Definition {
                        definitions: namespace.definitions,
                        index,
                    });
                }
            }
            current = namespace.definitions.outer;
        }
        None
    }
}

impl<'p, 's> Definition<'p, 's> {
    pub(super) fn plan(self) -> &'p CtePlan {
        &self.definitions.ctes[self.index]
    }

    pub(super) fn namespace(self) -> Option<Namespace<'p, 's>> {
        let ceiling = if self.plan().recursive {
            self.definitions.ctes.len()
        } else {
            self.index
        };
        if ceiling == 0 {
            self.definitions.outer
        } else {
            Some(Namespace {
                definitions: self.definitions,
                ceiling,
            })
        }
    }

    pub(super) fn state(self) -> ProducerState {
        self.definitions.producers[self.index].get()
    }
    pub(super) fn set_state(self, state: ProducerState) {
        self.definitions.producers[self.index].set(state);
    }
    pub(super) fn parent(self) -> Option<usize> {
        self.definitions.parent
    }
}
