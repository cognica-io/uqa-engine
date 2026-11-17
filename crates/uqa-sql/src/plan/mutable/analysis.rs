//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered preparation observes original addresses and installs only typed independently owned metadata.

use super::{MutableExecutablePlan, MutableQueryPlan, RewriteMemory};
use crate::{
    ast::FunctionBinding,
    catalog::work::Work,
    plan::{ProjectionPlan, QueryPlan, SharedAnalyzedSource, SourcePlan},
    RowSchema, SQLError,
};
use uqa_core::memory::{BudgetedVec, MemoryReservation, Produced, ProductionControl};

mod observation;
pub(crate) use observation::MetadataObserver;
#[cfg(test)]
mod tests;
mod traversal;

pub(crate) type ProjectionAliasUpdates = Produced<Vec<(usize, Produced<String>)>>;

#[derive(Default)]
pub(crate) struct QueryMetadataUpdate {
    pub root: Option<MutableQueryPlan>,
    pub output: Option<RowSchema>,
    pub source: Option<SharedAnalyzedSource>,
    pub grouping: Option<Produced<crate::semantics::grouping_sets::AnalyzedGroupingSets>>,
}

impl QueryMetadataUpdate {
    pub(super) fn install(
        self,
        query: &mut QueryPlan,
        mut memory: Option<&mut RewriteMemory>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if let Some(root) = self.root {
            root.assert_owner(control);
            let MutableQueryPlan {
                query: replacement,
                memory: mut replacement_memory,
            } = root;
            query.root = replacement.root;
            if let Some(memory) = memory.as_deref_mut() {
                *memory.field_mut("root") = replacement_memory
                    .as_mut()
                    .expect("replacement query owner")
                    .field_mut("root")
                    .take();
            }
        }
        if let Some(output) = self.output {
            query.analyzed_output = Some(output);
        }
        if let Some(source) = self.source {
            query.analyzed_source = Some(source);
        }
        if let Some(grouping) = self.grouping {
            let (grouping, lease) = grouping.into_parts();
            let (grouping, lease) = control.finish(grouping, lease)?.into_parts();
            query.analyzed_grouping = Some(grouping);
            if let Some(memory) = memory {
                let direct = lease.expect("admitted grouping metadata");
                *memory.field_mut("analyzed_grouping") = Some(RewriteMemory::Fields {
                    fields: BudgetedVec::new(direct.budget()),
                    direct,
                });
            }
        }
        Ok(())
    }
}

pub(crate) type SourceMetadataUpdate = Produced<Vec<(usize, Produced<FunctionBinding>)>>;
type ProjectionObserver<'a> =
    dyn FnMut(&[ProjectionPlan]) -> Result<ProjectionAliasUpdates, SQLError> + 'a;
pub(super) type QueryObserver<'a> =
    dyn FnMut(&QueryPlan) -> Result<QueryMetadataUpdate, SQLError> + 'a;
type SourceReplacement<'a> =
    dyn FnMut(&SourcePlan) -> Result<Option<super::MutableSourcePlan>, SQLError> + 'a;

struct Walker<'a, 'c> {
    query: Option<&'a mut QueryObserver<'a>>,
    projections: Option<&'a mut ProjectionObserver<'a>>,
    replacement: Option<&'a mut SourceReplacement<'a>>,
    control: ProductionControl<'c>,
    mark_relations: bool,
}

impl crate::plan::UnifiedPlan {
    /// Ordinary input replacement shares projection selection and installation with independently owned plans.
    pub(crate) fn apply_projection_aliases<'a>(
        &mut self,
        observe: &'a mut ProjectionObserver<'a>,
    ) -> Result<(), SQLError> {
        Walker {
            query: None,
            projections: Some(observe),
            replacement: None,
            control: ProductionControl::uncontrolled(),
            mark_relations: false,
        }
        .executable(self, None)
    }
}

impl MutableExecutablePlan {
    /// Catalog statement inputs retain their bound identities. CTE mutation targets are bound as well; the enclosing action target keeps its independent binding decision.
    pub fn mark_catalog_relations_bound(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if let Self::Command(command) = &self {
            if !matches!(
                &**command,
                crate::plan::CommandPlan::Insert(_)
                    | crate::plan::CommandPlan::Update(_)
                    | crate::plan::CommandPlan::Delete(_)
                    | crate::plan::CommandPlan::Merge(_)
                    | crate::plan::CommandPlan::Notify { .. }
            ) {
                return Err(SQLError::Internal(
                    "catalog-owned statement lowered to an unsupported command".into(),
                ));
            }
        }
        Walker {
            query: None,
            projections: None,
            replacement: None,
            control: *control,
            mark_relations: true,
        }
        .plan(&mut self)?;
        Ok(self)
    }
    /// Select independently owned replacements while borrowing original sources in SQL source order. Replacement payloads and constructor records are installed together before visiting descendants.
    pub fn replace_sources<'a>(
        &mut self,
        control: &ProductionControl<'_>,
        replace: &'a mut SourceReplacement<'a>,
    ) -> Result<(), SQLError> {
        Walker {
            query: None,
            projections: None,
            replacement: Some(replace),
            control: *control,
            mark_relations: false,
        }
        .plan(self)
    }
    /// Preparation owns this private destination through the call; failure discards it before publication. Observers only receive original read-only syntax and return typed owning updates.
    pub(crate) fn apply_query_metadata<'a>(
        &mut self,
        control: &ProductionControl<'_>,
        observe: &'a mut QueryObserver<'a>,
    ) -> Result<(), SQLError> {
        Walker {
            query: Some(observe),
            projections: None,
            replacement: None,
            mark_relations: false,
            control: *control,
        }
        .plan(self)
    }
}

async fn install_source_updates<W: Work>(
    source: &mut SourcePlan,
    mut memory: Option<&mut RewriteMemory>,
    updates: SourceMetadataUpdate,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    let (values, buffer) = updates.into_parts();
    struct Updates {
        values: std::vec::IntoIter<(usize, Produced<FunctionBinding>)>,
        _buffer: Option<MemoryReservation>,
    }
    let mut updates = Updates {
        values: values.into_iter(),
        _buffer: buffer,
    };
    for (index, selected) in &mut updates.values {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        let (binding, owner) = match source {
            SourcePlan::Function { binding, .. } => {
                assert_eq!(index, 0, "single function source binding");
                (
                    binding,
                    memory
                        .as_deref_mut()
                        .map(|memory| memory.field_mut("binding")),
                )
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                let owner = memory.as_deref_mut().map(|memory| {
                    memory
                        .field_mut("functions")
                        .as_mut()
                        .expect("function vector")
                        .item_mut(index)
                        .as_mut()
                        .expect("table function")
                        .field_mut("binding")
                });
                (&mut functions[index].binding, owner)
            }
            _ => {
                return Err(SQLError::Internal(
                    "selected function binding lost its source".into(),
                ));
            }
        };
        let (selected, lease) = selected.into_parts();
        let selected = control.finish(selected, lease)?;
        let (selected, lease) = selected.into_parts();
        *binding = Some(selected);
        if let Some(owner) = owner {
            let direct = lease.expect("admitted selected function binding");
            *owner = Some(RewriteMemory::Fields {
                fields: BudgetedVec::new(direct.budget()),
                direct,
            });
        }
    }
    Ok(())
}

async fn install_aliases<W: Work>(
    projections: &mut [ProjectionPlan],
    mut memory: Option<&mut RewriteMemory>,
    aliases: Produced<Vec<(usize, Produced<String>)>>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    let (values, buffer) = aliases.into_parts();
    struct Aliases {
        values: std::vec::IntoIter<(usize, Produced<String>)>,
        _buffer: Option<MemoryReservation>,
    }
    let mut aliases = Aliases {
        values: values.into_iter(),
        _buffer: buffer,
    };
    for (index, alias) in &mut aliases.values {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        let (alias, lease) = alias.into_parts();
        let (alias, lease) = control.finish(alias, lease)?.into_parts();
        projections[index].alias = Some(alias);
        if let Some(memory) = memory.as_deref_mut() {
            let direct = lease.expect("admitted projection alias");
            *memory
                .item_mut(index)
                .as_mut()
                .expect("projection")
                .field_mut("alias") = Some(RewriteMemory::Fields {
                fields: BudgetedVec::new(direct.budget()),
                direct,
            });
        }
    }
    Ok(())
}
