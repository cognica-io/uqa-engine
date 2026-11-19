//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate captured named builtin sequence arguments without evaluating numeric or dynamic inputs.

use super::routine_dependencies::{self, Reference, References};
use crate::catalog::{relation_names::RelationNameCatalog, CatalogReadView};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    catalog::CatalogWork,
    plan::UnifiedPlan,
    schema::dependencies::rewrites::{
        visit_stored_sequence_references_with_work, SequenceFunctionRelations, StoredSequenceInput,
    },
    SQLError,
};

pub fn validate(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    crate::catalog::selection_work::inline(validate_with_work(catalog, control, &mut ()))
}

pub(super) async fn validate_with_work<W: CatalogWork + Send>(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    control.check()?;
    routine_dependencies::visit(
        catalog.snapshot(),
        &SequenceReferences { catalog, control },
        control,
        work,
    )
    .await
}

struct SequenceReferences<'a, 'b> {
    catalog: &'a CatalogReadView,
    control: &'a ProductionControl<'b>,
}

impl SequenceFunctionRelations for SequenceReferences<'_, '_> {
    async fn reference<W: CatalogWork + Send>(
        &self,
        reference: &str,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let Some(identity) = uqa_sql::schema::sequences::native::function_relation_with_work(
            reference,
            self.control,
            work,
        )
        .await?
        else {
            return Ok(());
        };
        let snapshot = self.catalog.snapshot();
        let names = RelationNameCatalog::new(&snapshot.tables, &snapshot.definitions);
        // regclass accepts any relation kind; the sequence operation checks its kind at execution.
        if names.kind_at(&identity).is_none()
            && self
                .catalog
                .graph_label_kind(&identity.schema, &identity.name)?
                .is_none()
        {
            return Err(SQLError::Internal(
                "missing native sequence function relation".into(),
            ));
        }
        self.control.check()?;
        Ok(())
    }
}

impl References for SequenceReferences<'_, '_> {
    async fn inspect<W: CatalogWork + Send>(
        &self,
        reference: Reference<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let plan;
        let input = match reference {
            Reference::Expression(expr) => StoredSequenceInput::Expression(expr),
            Reference::Scalar(expr) => StoredSequenceInput::Scalar(expr),
            Reference::Plan(plan) => StoredSequenceInput::Plan(plan),
            Reference::Statement(statement) => {
                plan = UnifiedPlan::lower_borrowed_for_rewrite(
                    statement,
                    &|_: &str| false,
                    self.control,
                )?;
                StoredSequenceInput::Plan(plan.as_plan_ref())
            }
            // These carry no argument expression. The independent routine predicate checks their concrete identities.
            Reference::Binding(_) | Reference::Routine(..) | Reference::Trigger(..) => {
                return Ok(())
            }
        };
        visit_stored_sequence_references_with_work(input, self.control, work, self).await
    }
}

#[cfg(test)]
mod tests;
