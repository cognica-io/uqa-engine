//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query layout restoration and input application select the same original observations.

use super::super::{AnalysisIdentity, QuerySchemas, RewrittenBlocks};
use crate::{
    catalog::work::Work,
    plan::{
        MetadataObserver, QueryLayoutUpdate, QueryMetadataUpdate, QueryPlan, RelationalPlan,
        SharedAnalyzedSource,
    },
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub(super) struct QueryLayouts<'a>(pub &'a mut QuerySchemas, pub &'a mut RewrittenBlocks);

impl<W: Work + Send> MetadataObserver<W> for QueryLayouts<'_> {
    fn visit_table_queries(&self) -> bool {
        true
    }

    async fn query(
        &mut self,
        query: &QueryPlan,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<QueryMetadataUpdate>, SQLError> {
        let root = match &query.root {
            RelationalPlan::QueryBlock(block) => {
                self.1.remove(&AnalysisIdentity::from(block.as_ref()))
            }
            _ => None,
        };
        let selected = root.as_deref().unwrap_or(query);
        let source = match &selected.root {
            RelationalPlan::QueryBlock(block) => match &block.from {
                Some(source) => self.0.take_source_from(source, control, work).await?,
                None => None,
            },
            _ => None,
        };
        Ok(Some(QueryMetadataUpdate {
            root: root.map(Into::into),
            layouts: Some(QueryLayoutUpdate {
                output: self.0.outputs.remove(&AnalysisIdentity::from(query)),
                source: source
                    .map(|source| SharedAnalyzedSource::from_produced(source, control))
                    .transpose()?,
            }),
            ..Default::default()
        }))
    }
}
