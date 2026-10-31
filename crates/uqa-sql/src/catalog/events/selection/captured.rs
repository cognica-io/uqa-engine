//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An active-rule selection retains its catalog generation instead of copying stored action trees.

use super::{RuleCatalog, RuleEvent, StoredRule};
use crate::SQLError;
use std::sync::Arc;
use uqa_core::{
    memory::{OwnedProductionControl, Produced, ProductionControl},
    RelationIdentity,
};

enum Definitions {
    Catalog {
        rules: Arc<RuleCatalog>,
        relation: Produced<RelationIdentity>,
        event: RuleEvent,
        replica: bool,
    },
    External(Vec<StoredRule>),
}

/// The selection freezes the firing role and registry generation at the same boundary as an owned rule list. Each iteration borrows declarations from that generation and observes both invoking cancellation handles.
pub struct CapturedRules {
    definitions: Definitions,
    control: OwnedProductionControl,
}

impl CapturedRules {
    pub fn from_catalog(
        rules: Arc<RuleCatalog>,
        relation: Produced<RelationIdentity>,
        event: RuleEvent,
        replica: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let (relation, memory) = relation.into_parts();
        let relation = control.finish(relation, memory)?;
        Ok(Self {
            definitions: Definitions::Catalog {
                rules,
                relation,
                event,
                replica,
            },
            control: control.capture(),
        })
    }

    /// Compatibility for external catalogs that supply an already selected, independently owned declaration list. Native catalogs retain their immutable registry directly.
    pub fn from_external(
        rules: Vec<StoredRule>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        Ok(Self {
            definitions: Definitions::External(rules),
            control: control.capture(),
        })
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = Result<&StoredRule, SQLError>> {
        let (candidates, selection) = match &self.definitions {
            Definitions::Catalog {
                rules,
                relation,
                event,
                replica,
            } => (
                Candidates::Catalog(rules.get(&**relation).map(|rules| rules.values())),
                Some((*event, *replica)),
            ),
            Definitions::External(rules) => (Candidates::External(rules.iter()), None),
        };
        RuleIter {
            candidates,
            selection,
            control: self.control.control(),
            finished: false,
        }
    }

    pub fn is_empty(&self) -> Result<bool, SQLError> {
        self.control.control().check()?;
        Ok(self.iter().next().transpose()?.is_none())
    }

    pub fn any(&self, mut predicate: impl FnMut(&StoredRule) -> bool) -> Result<bool, SQLError> {
        for rule in self.iter() {
            if predicate(rule?) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

enum Candidates<'a> {
    Catalog(Option<std::collections::btree_map::Values<'a, String, StoredRule>>),
    External(std::slice::Iter<'a, StoredRule>),
}
struct RuleIter<'a> {
    candidates: Candidates<'a>,
    selection: Option<(RuleEvent, bool)>,
    control: ProductionControl<'a>,
    finished: bool,
}
impl<'a> RuleIter<'a> {
    fn next_rule(&mut self, reverse: bool) -> Option<Result<&'a StoredRule, SQLError>> {
        if self.finished {
            return None;
        }
        loop {
            if let Err(error) = self.control.check() {
                self.finished = true;
                return Some(Err(error.into()));
            }
            let candidate = match &mut self.candidates {
                Candidates::Catalog(values) => values.as_mut().and_then(|values| {
                    if reverse {
                        values.next_back()
                    } else {
                        values.next()
                    }
                }),
                Candidates::External(values) => {
                    if reverse {
                        values.next_back()
                    } else {
                        values.next()
                    }
                }
            };
            let Some(rule) = candidate else {
                self.finished = true;
                return None;
            };
            let selected = self.selection.is_none_or(|(event, replica)| {
                rule.definition.event == event
                    && if replica {
                        rule.enabled.fires_in_replica()
                    } else {
                        rule.enabled.fires_in_origin()
                    }
            });
            if selected {
                return Some(Ok(rule));
            }
        }
    }
}
impl<'a> Iterator for RuleIter<'a> {
    type Item = Result<&'a StoredRule, SQLError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_rule(false)
    }
}
impl DoubleEndedIterator for RuleIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.next_rule(true)
    }
}

#[cfg(test)]
mod tests;
