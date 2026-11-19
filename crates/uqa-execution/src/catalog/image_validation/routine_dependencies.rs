//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join concrete routine references from one native catalog without compiling deferred bodies.

use crate::catalog::{CatalogDefinitionSnapshot, CatalogReadSnapshot};
use std::future::Future;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    ast::{ColumnDef, FunctionBody},
    catalog::CatalogWork,
    plan::PlanRef,
    routines::{
        native::NativeRoutineReferences, CompiledFunctionBody, RoutineBody, SQLUserFunction,
    },
    SQLError,
};

pub(super) use uqa_sql::routines::native::StoredRoutineReference as Reference;

pub(super) trait References: Sync {
    fn inspect<W: CatalogWork + Send>(
        &self,
        reference: Reference<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

impl References for NativeRoutineReferences<'_> {
    async fn inspect<W: CatalogWork + Send>(
        &self,
        reference: Reference<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.inspect_with_work(reference, work).await
    }
}

pub fn validate(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    crate::catalog::selection_work::inline(validate_with_work(
        snapshot,
        memory,
        cancellation,
        &mut (),
    ))
}

fn declaration(function: &uqa_sql::routines::SQLRoutine) -> &uqa_sql::ast::CreateFunction {
    &function.def
}
pub(super) fn group(
    routines: &[uqa_sql::routines::SQLRoutine],
) -> impl Iterator<Item = &uqa_sql::ast::CreateFunction> {
    routines.iter().map(declaration)
}

pub(super) async fn validate_with_work<W: CatalogWork + Send>(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut W,
) -> Result<(), SQLError> {
    let d = &snapshot.definitions;
    let control = ProductionControl::new(memory, cancellation, cancellation);
    for overloads in d.sql_user_functions.values() {
        work.checkpoint().await?;
        for routine in overloads {
            work.checkpoint().await?;
            uqa_sql::routines::native::validate_captured_body_with_work(routine, &control, work)
                .await?;
        }
    }
    let references = references_with_work(d, memory, cancellation, work).await?;
    visit(snapshot, &references, &control, work).await?;
    references.retire_with_work(work).await
}

pub(super) async fn references_with_work<'a>(
    definitions: &'a CatalogDefinitionSnapshot,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    work: &mut (impl CatalogWork + Send),
) -> Result<NativeRoutineReferences<'a>, SQLError> {
    let mut groups = definitions
        .sql_user_functions
        .values()
        .map(Vec::as_slice)
        .map(group);
    NativeRoutineReferences::from_groups_with_work(&mut groups, memory, cancellation, work).await
}

pub(super) async fn visit<W: CatalogWork + Send>(
    snapshot: &CatalogReadSnapshot,
    references: &impl References,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    let d = &snapshot.definitions;
    let mut fields = Fields {
        references,
        control: *control,
        work,
    };
    fields.tables(snapshot).await?;
    fields.foreign_tables(d).await?;
    fields.domains(d).await?;
    fields.views(d).await?;
    fields.functions(d).await?;
    fields.triggers(d).await?;
    fields.rules(&d.rules).await?;
    control.check()?;
    Ok(())
}

struct Fields<'a, 'c, R, W> {
    references: &'a R,
    control: ProductionControl<'c>,
    work: &'a mut W,
}

impl<R: References, W: CatalogWork + Send> Fields<'_, '_, R, W> {
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.control.check()?;
        self.work.checkpoint().await?;
        self.control.check()?;
        Ok(())
    }

    async fn visit(&mut self, reference: Reference<'_>) -> Result<(), SQLError> {
        self.checkpoint().await?;
        self.references.inspect(reference, self.work).await
    }

    async fn tables(&mut self, snapshot: &CatalogReadSnapshot) -> Result<(), SQLError> {
        for table in snapshot.tables.values() {
            self.checkpoint().await?;
            self.columns(&table.columns).await?;
            for check in table.checks.iter() {
                self.visit(Reference::Expression(&check.expr)).await?;
            }
            if let Some(spec) = &table.hierarchy.partition_spec {
                for key in &spec.keys {
                    self.visit(Reference::Expression(key)).await?;
                }
            }
        }
        Ok(())
    }

    async fn foreign_tables(&mut self, d: &CatalogDefinitionSnapshot) -> Result<(), SQLError> {
        for table in d.foreign_tables.values() {
            self.checkpoint().await?;
            self.columns(&table.columns).await?;
            for check in &table.checks {
                self.visit(Reference::Expression(&check.expr)).await?;
            }
        }
        Ok(())
    }

    async fn domains(&mut self, d: &CatalogDefinitionSnapshot) -> Result<(), SQLError> {
        for domain in d.domains.values() {
            self.checkpoint().await?;
            if let Some(default) = &domain.definition.default {
                self.visit(Reference::Expression(default)).await?;
            }
            for check in &domain.definition.checks {
                self.visit(Reference::Expression(&check.expression)).await?;
            }
        }
        Ok(())
    }

    async fn views(&mut self, d: &CatalogDefinitionSnapshot) -> Result<(), SQLError> {
        for view in d.views.values() {
            self.visit(Reference::Plan(PlanRef::Query(&view.query)))
                .await?;
        }
        Ok(())
    }

    async fn functions(&mut self, d: &CatalogDefinitionSnapshot) -> Result<(), SQLError> {
        for overloads in d.sql_user_functions.values() {
            self.checkpoint().await?;
            for function in overloads {
                self.function(function).await?;
            }
        }
        Ok(())
    }

    async fn triggers(&mut self, d: &CatalogDefinitionSnapshot) -> Result<(), SQLError> {
        for triggers in d.triggers.values() {
            self.checkpoint().await?;
            for trigger in triggers.values() {
                self.visit(Reference::Trigger(
                    &trigger.definition.function,
                    trigger.function_object_id,
                ))
                .await?;
                if let Some(condition) = &trigger.definition.when {
                    self.visit(Reference::Expression(condition)).await?;
                }
            }
        }
        Ok(())
    }

    async fn columns(&mut self, columns: &[ColumnDef]) -> Result<(), SQLError> {
        for column in columns {
            self.checkpoint().await?;
            for expression in column.default.iter().chain(&column.check) {
                self.visit(Reference::Expression(expression)).await?;
            }
            if let Some(generated) = &column.generated {
                self.visit(Reference::Expression(&generated.expression))
                    .await?;
                for binding in &generated.function_dependencies {
                    self.visit(Reference::Binding(binding)).await?;
                }
            }
        }
        Ok(())
    }

    async fn function(&mut self, function: &SQLUserFunction) -> Result<(), SQLError> {
        self.checkpoint().await?;
        for parameter in &function.def.params {
            self.checkpoint().await?;
            if let Some(default) = &parameter.default {
                self.visit(Reference::Expression(default)).await?;
            }
        }
        if let FunctionBody::Statements(statements) = &function.def.body {
            for statement in statements {
                self.visit(Reference::Statement(statement)).await?;
            }
        }
        if let RoutineBody::Bound(body) = &function.body {
            let CompiledFunctionBody::SQL(plans) = &***body else {
                return Err(SQLError::Internal(
                    "native catalog contains a session routine body".into(),
                ));
            };
            for plan in plans {
                self.visit(Reference::Plan(plan.into())).await?;
            }
        }
        Ok(())
    }

    async fn rules(
        &mut self,
        catalog: &uqa_sql::catalog::events::RuleCatalog,
    ) -> Result<(), SQLError> {
        for rules in catalog.values() {
            self.checkpoint().await?;
            for rule in rules.values() {
                self.checkpoint().await?;
                for action in &rule.definition.actions {
                    self.visit(Reference::Statement(action)).await?;
                }
                if let Some(dependencies) = &rule.dependencies {
                    for routine in &dependencies.routines {
                        self.checkpoint().await?;
                        if let Some(identity) = routine.object_id {
                            self.visit(Reference::Routine(identity, &routine.argument_types))
                                .await?;
                        }
                    }
                }
                if let Some(plan) = &rule.condition_plan {
                    self.visit(Reference::Scalar(&plan.scalar)).await?;
                    for query in &plan.subqueries {
                        self.visit(Reference::Plan(PlanRef::Query(query))).await?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
