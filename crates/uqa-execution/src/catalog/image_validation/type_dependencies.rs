//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog type closure joins original declarations and physical row descriptors.

use super::super::{foreign::StoredForeignTable, CatalogReadSnapshot};
use uqa_core::{image_io::AsyncImageInput, memory::MemoryBudget, CancellationToken};
use uqa_sql::{
    ast::{FunctionBody, FunctionReturns, RoutineDefaultType},
    catalog::{
        type_references::{TypeReferenceInput, TypeReferences},
        CatalogWork,
    },
    routines::{CompiledFunctionBody, RoutineBody},
    SQLError,
};

pub(super) async fn references_with_work<'a>(
    snapshot: &'a CatalogReadSnapshot,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    work: &mut (impl CatalogWork + Send),
) -> Result<TypeReferences<'a>, SQLError> {
    let d = &snapshot.definitions;
    let rows = snapshot
        .tables
        .iter()
        .map(|(_, table)| table.catalog_oids)
        .chain(d.views.values().map(|view| view.relation_oids()))
        .chain(
            d.foreign_tables
                .values()
                .map(StoredForeignTable::relation_oids),
        );
    TypeReferences::new_with_work(
        TypeReferenceInput {
            domains: &d.domains,
            enums: &d.enums,
            composites: &d.composites,
            ranges: &d.ranges,
            shells: &d.shells,
        },
        rows,
        memory,
        cancellation,
        work,
    )
    .await
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

pub(super) async fn from_input(
    snapshot: &CatalogReadSnapshot,
    cancellation: &CancellationToken,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate_with_work(snapshot, input.memory, cancellation, input).await
}

async fn validate_with_work(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    let d = &snapshot.definitions;
    let types = references_with_work(snapshot, memory, cancellation, work).await?;
    for overloads in d.sql_user_functions.values() {
        types.checkpoint(work).await?;
        for function in overloads {
            types.checkpoint(work).await?;
            for parameter in &function.def.params {
                types.checkpoint(work).await?;
                types
                    .stored_name_with_work(&parameter.type_name, work)
                    .await?;
                if let Some(default) = &parameter.default {
                    types.expression_with_work(default, work).await?;
                }
                if let Some(RoutineDefaultType::Concrete(ty)) = &parameter.default_type {
                    types.column_type_with_work(ty, work).await?;
                }
            }
            if let FunctionBody::Statements(statements) = &function.def.body {
                for statement in statements {
                    types.checkpoint(work).await?;
                    types.statement_with_work(statement, work).await?;
                }
            }
            if let RoutineBody::Bound(body) = &function.body {
                if let CompiledFunctionBody::SQL(plans) = &***body {
                    for plan in plans {
                        types.checkpoint(work).await?;
                        types.plan_with_work(plan.into(), work).await?;
                    }
                }
            }
            match &function.def.returns {
                FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
                    types.stored_name_with_work(type_name, work).await?;
                }
                FunctionReturns::None | FunctionReturns::Table => {}
            }
        }
    }
    validate_membership(snapshot, &types, work).await?;
    cancellation.check()?;
    types.retire(work).await
}

async fn validate_membership(
    snapshot: &CatalogReadSnapshot,
    types: &TypeReferences<'_>,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    let d = &snapshot.definitions;
    let mut graph = types.membership();
    for (_, table) in snapshot.tables.iter() {
        types.checkpoint(work).await?;
        for column in table.columns.iter() {
            graph
                .member(table.catalog_oids.reltype(), &column.ty, work)
                .await?;
        }
    }
    for table in d.foreign_tables.values() {
        types.checkpoint(work).await?;
        for column in &table.columns {
            graph
                .member(table.relation_oids().reltype(), &column.ty, work)
                .await?;
        }
    }
    for domain in d.domains.values() {
        graph
            .member(domain.oid, &domain.definition.base, work)
            .await?;
    }
    for composite in d.composites.values() {
        types.checkpoint(work).await?;
        for attribute in &composite.attributes {
            if attribute.dropped {
                types.checkpoint(work).await?;
            } else {
                graph.member(composite.oid, &attribute.ty, work).await?;
            }
        }
    }
    for range in d.ranges.values() {
        graph.member(range.oid, &range.subtype, work).await?;
        graph
            .member(range.multirange.oid, &range.subtype, work)
            .await?;
    }
    for view in d.views.values() {
        types.checkpoint(work).await?;
        for column in &view.materialized_column_types {
            if let Some(ty) = column {
                graph
                    .member(view.relation_oids().reltype(), ty, work)
                    .await?;
            } else {
                types.checkpoint(work).await?;
            }
        }
    }
    graph.finish(work).await
}

#[cfg(test)]
mod tests;
