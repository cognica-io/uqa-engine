//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fusion eligibility observes borrowed names without retaining qualifier strings or leaf lists.

use super::{classify_signal, contains_explicit_fusion, RetrievalSignalKind, ScalarExpr};

#[derive(Default)]
pub(super) struct Signals<'a> {
    text: bool,
    vector: bool,
    qualifier: Qualifier<'a>,
    mismatch: bool,
    explicit: bool,
}

#[derive(Default, PartialEq, Eq)]
enum Qualifier<'a> {
    #[default]
    Unobserved,
    Unqualified,
    Qualified(&'a str),
}

impl<'a> Signals<'a> {
    pub(super) fn observe(&mut self, expression: &'a ScalarExpr) {
        self.explicit |= contains_explicit_fusion(expression);
        let Some((kind, qualifier)) = classify_signal(expression) else {
            return;
        };
        self.text |= kind == RetrievalSignalKind::Text;
        self.vector |= kind == RetrievalSignalKind::Vector;
        let qualifier = qualifier.map_or(Qualifier::Unqualified, Qualifier::Qualified);
        if self.qualifier == Qualifier::Unobserved {
            self.qualifier = qualifier;
        } else {
            self.mismatch |= self.qualifier != qualifier;
        }
    }

    pub(super) fn observe_conjunction(&mut self, expression: &'a ScalarExpr) {
        match expression {
            ScalarExpr::And(children) => {
                for child in children {
                    self.observe_conjunction(child);
                }
            }
            _ => self.observe(expression),
        }
    }

    pub(super) fn can_fuse(&self, allow_unqualified_signals: bool) -> bool {
        self.text
            && self.vector
            && !self.explicit
            && !self.mismatch
            && match self.qualifier {
                Qualifier::Qualified(_) => true,
                Qualifier::Unqualified => allow_unqualified_signals,
                Qualifier::Unobserved => false,
            }
    }
}
