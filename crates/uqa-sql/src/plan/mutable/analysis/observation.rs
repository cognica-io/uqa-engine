//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed metadata producers borrow stationary syntax; only the plan owner installs their returned allocations.

use super::{ProjectionAliasUpdates, QueryMetadataUpdate, SourceMetadataUpdate, Walker};
use crate::{
    catalog::work::Work,
    plan::{MutableSourcePlan, ProjectionPlan, QueryPlan, SourcePlan},
    SQLError,
};
use std::future::{ready, Future};
use uqa_core::memory::ProductionControl;

pub(crate) trait MetadataObserver<W: Work> {
    fn mark_relations(&self) -> bool {
        false
    }
    fn visit_table_queries(&self) -> bool {
        false
    }
    fn query(
        &mut self,
        _: &QueryPlan,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl Future<Output = Result<Option<QueryMetadataUpdate>, SQLError>> + Send {
        ready(Ok(None))
    }
    fn source_replacement(
        &mut self,
        _: &SourcePlan,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl Future<Output = Result<Option<MutableSourcePlan>, SQLError>> + Send {
        ready(Ok(None))
    }
    fn source(
        &mut self,
        _: &SourcePlan,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl Future<Output = Result<Option<SourceMetadataUpdate>, SQLError>> + Send {
        ready(Ok(None))
    }
    fn projections(
        &mut self,
        _: &[ProjectionPlan],
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl Future<Output = Result<Option<ProjectionAliasUpdates>, SQLError>> + Send {
        ready(Ok(None))
    }
}

impl<W: Work> MetadataObserver<W> for Walker<'_, '_> {
    fn mark_relations(&self) -> bool {
        self.mark_relations
    }
    fn visit_table_queries(&self) -> bool {
        self.query.is_some() || self.projections.is_some()
    }
    fn query(
        &mut self,
        query: &QueryPlan,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl Future<Output = Result<Option<QueryMetadataUpdate>, SQLError>> + Send {
        ready(
            self.query
                .as_mut()
                .map(|observe| observe(query))
                .transpose(),
        )
    }
    fn source_replacement(
        &mut self,
        source: &SourcePlan,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl Future<Output = Result<Option<MutableSourcePlan>, SQLError>> + Send {
        ready(match &mut self.replacement {
            Some(replace) => replace(source),
            None => Ok(None),
        })
    }
    fn projections(
        &mut self,
        projections: &[ProjectionPlan],
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl Future<Output = Result<Option<ProjectionAliasUpdates>, SQLError>> + Send {
        ready(
            self.projections
                .as_mut()
                .map(|observe| observe(projections))
                .transpose(),
        )
    }
}
