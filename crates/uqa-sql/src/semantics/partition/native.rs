//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate captured partition declarations without transforming or evaluating their bounds.

use crate::{
    ast::{ColumnDef, PartitionSpec, PartitionStrategy, TableCheck, TableHierarchy},
    catalog::{
        resolution::bound_relation_identity_with_control, scalar_bindings::NativeScalarContext,
    },
    ColumnType, RowSchema, SQLError,
};
use uqa_core::{
    memory::{BudgetedMap, MemoryBudget, Produced, ProductionControl, ProductionVec},
    CancellationToken, RelationIdentity,
};

mod bounds;
mod values;

#[derive(Clone, Copy)]
pub struct NativePartitionTable<'a> {
    pub name: &'a RelationIdentity,
    pub columns: &'a [ColumnDef],
    pub hierarchy: &'a TableHierarchy,
    pub checks: &'a [TableCheck],
}

pub struct NativePartitionContext<'a, 'b> {
    pub scalar: NativeScalarContext<'a, 'b>,
    pub values: Option<&'a dyn crate::expr::SQLValueCatalog>,
}

/// Graph membership is checked by the inheritance owner. This predicate checks the original key scopes, captured constants, sibling disjointness and retained detached-partition conditions. It never installs a parent edge, coerces a value or invokes a stored expression.
pub fn validate<'a>(
    tables: impl Iterator<Item = NativePartitionTable<'a>>,
    context: &NativePartitionContext<'_, '_>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let mut relations = BudgetedMap::new(memory);
    let mut children = BudgetedMap::new(memory);
    for table in tables {
        control.check()?;
        if relations.insert(table.name, table)?.is_some() {
            return Err(invalid("duplicate relation"));
        }
        if table.hierarchy.partition_bound.is_some() {
            let [parent] = table.hierarchy.parents.as_slice() else {
                return Err(invalid("partition must have one parent"));
            };
            let parent = bound_relation_identity_with_control(parent, &control)?;
            children.insert(table.name, parent)?;
        }
    }
    for (_, table) in &relations {
        control.check()?;
        if let Some(spec) = &table.hierarchy.partition_spec {
            let types = key_types(table, spec, context, &control)?;
            let mut siblings: ProductionVec<'_, &crate::ast::PartitionBound> =
                ProductionVec::new(control);
            for (child, parent) in &children {
                control.check()?;
                if **parent == *table.name {
                    let child = relations.get(child).expect("captured child");
                    let bound = child.hierarchy.partition_bound.as_ref().expect("partition");
                    crate::schema::inheritance::alter::validate_row_type_with_control(
                        table.columns,
                        child.columns,
                        &table.name.name,
                        &child.name.name,
                        true,
                        false,
                        &control,
                    )?;
                    bounds::validate(spec, &types, bound, context, &control)?;
                    for previous in &*siblings {
                        bounds::disjoint(previous, bound, &types, context, &control)?;
                    }
                    siblings.push_copy(bound)?;
                }
            }
        }
        for check in table.checks {
            control.check()?;
            if let Some(detached) = &check.partition_constraint {
                let types = key_types(table, &detached.spec, context, &control)?;
                bounds::validate(&detached.spec, &types, &detached.bound, context, &control)?;
            }
        }
    }
    for (_, parent) in &children {
        control.check()?;
        if relations
            .get(&**parent)
            .is_none_or(|table| table.hierarchy.partition_spec.is_none())
        {
            return Err(invalid("missing partitioned parent"));
        }
    }
    control.check()?;
    Ok(())
}

fn key_types(
    table: &NativePartitionTable<'_>,
    spec: &PartitionSpec,
    context: &NativePartitionContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ColumnType>>, SQLError> {
    control.check()?;
    if spec.keys.is_empty()
        || spec.keys.len() > crate::schema::inheritance::PARTITION_MAX_KEYS
        || (spec.strategy == PartitionStrategy::List && spec.keys.len() != 1)
    {
        return Err(invalid("partition key width"));
    }
    let schema = RowSchema::with_column_metadata_with_control(
        Some(&table.name.name),
        table
            .columns
            .iter()
            .map(|column| (column.name.as_str(), Some(&column.ty), column.collation_oid)),
        control,
    )?;
    let mut types = ProductionVec::new(*control);
    for key in &spec.keys {
        control.check()?;
        crate::schema::inheritance::validate_stored_key_references(key, table.columns, control)?;
        let ty = crate::catalog::scalar_bindings::expression_type(
            key,
            &schema,
            &context.scalar,
            control,
        )?
        .ok_or_else(|| invalid("unresolved partition key type"))?;
        context.scalar.types.column_type(&ty)?;
        super::validate_partition_key_type(spec.strategy, &ty)?;
        types.push_produced(ty)?;
    }
    Ok(types.finish()?)
}

fn invalid(detail: &str) -> SQLError {
    SQLError::Internal(format!("invalid native partition binding: {detail}"))
}

#[cfg(test)]
mod tests;
