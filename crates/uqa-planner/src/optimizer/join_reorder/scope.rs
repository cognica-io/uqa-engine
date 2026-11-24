//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A lexical CTE shadows a physical index even when its output uses the same column names.

use super::{SourcePlan, SourceStatistics};
use crate::{CtePlan, LocalAccessEstimate, RelationStats};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    binding::production::BindingNameSet, catalog::index::lookup::LookupColumn, SQLError, ScalarExpr,
};

pub(super) struct JoinStatistics<'a> {
    parent: &'a dyn SourceStatistics,
    names: BindingNameSet,
}

impl<'a> JoinStatistics<'a> {
    pub(super) fn new(
        parent: &'a dyn SourceStatistics,
        ctes: &[CtePlan],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut names = BindingNameSet::new();
        for cte in ctes {
            names.insert_with_control(&cte.name, control)?;
        }
        Ok(Self { parent, names })
    }

    pub(super) fn selected(&self) -> &dyn SourceStatistics {
        if self.names.is_empty() {
            self.parent
        } else {
            self
        }
    }
}

impl SourceStatistics for JoinStatistics<'_> {
    fn relation_statistics(&self, name: &str) -> Option<RelationStats> {
        self.parent.relation_statistics(name)
    }
    fn source_access_estimate(&self, source: &SourcePlan) -> Option<LocalAccessEstimate> {
        self.parent.source_access_estimate(source)
    }
    fn local_access_estimate(
        &self,
        name: &str,
        predicate: &ScalarExpr,
    ) -> Option<LocalAccessEstimate> {
        self.parent.local_access_estimate(name, predicate)
    }
    fn lookup_column(&self, name: &str, field: &str) -> Option<LookupColumn> {
        if uqa_sql::semantics::cte_reference_name(name)
            .is_some_and(|name| self.names.contains(&name))
        {
            return None;
        }
        self.parent.lookup_column(name, field)
    }
}
