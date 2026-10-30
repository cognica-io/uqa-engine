//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Target and source substitutions preserve qualified shadowing and first virtual-column fallback.

use super::{capture::whole_rows, input_expression, RuleMutation};
use crate::{
    ast::{ColumnDef, InternalRelationId},
    plan::ExpressionPlan,
    semantics::view_rewrite::DeferredOuterRewrite,
    ColumnIdentity, RowSchema, SQLError,
};
use uqa_core::memory::{OwnedMap, Produced, ProductionControl};

type Names = OwnedMap<Produced<ColumnIdentity>, Produced<ExpressionPlan>>;

pub(super) fn input_rewrite(
    statement: RuleMutation<'_>,
    definitions: &[ColumnDef],
    source_schema: Option<&RowSchema>,
    relation: InternalRelationId,
    source_relation: InternalRelationId,
    target_schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<DeferredOuterRewrite, SQLError> {
    let mut names = OwnedMap::new();
    insert(
        &mut names,
        None,
        statement.target_qualifier(),
        relation,
        definitions.len(),
        true,
        control,
    )?;
    for (position, column) in definitions.iter().enumerate() {
        insert(
            &mut names,
            Some(statement.target_qualifier()),
            &column.name,
            relation,
            position,
            true,
            control,
        )?;
        insert(
            &mut names,
            None,
            &column.name,
            relation,
            position,
            true,
            control,
        )?;
    }
    virtual_names(
        &mut names,
        target_schema,
        relation,
        definitions.len() + 1,
        control,
    )?;
    if let Some(schema) = source_schema {
        let whole_rows = whole_rows(schema, *control)?;
        for (position, (qualifier, ())) in whole_rows.iter().enumerate() {
            insert(
                &mut names,
                None,
                qualifier,
                source_relation,
                schema.len() + position,
                false,
                control,
            )?;
        }
        for (position, identity) in schema.identities().iter().enumerate() {
            insert(
                &mut names,
                identity.qualifier(),
                identity.column(),
                source_relation,
                position,
                true,
                control,
            )?;
            if schema.unqualified_position(identity.column()) == Some(position)
                && !definitions
                    .iter()
                    .any(|column| column.name == identity.column())
            {
                insert(
                    &mut names,
                    None,
                    identity.column(),
                    source_relation,
                    position,
                    true,
                    control,
                )?;
            }
        }
        virtual_names(
            &mut names,
            schema,
            source_relation,
            schema.len() + whole_rows.len(),
            control,
        )?;
    }
    DeferredOuterRewrite::from_inputs_owned(names, control)
}

fn insert(
    names: &mut Names,
    qualifier: Option<&str>,
    column: &str,
    relation: InternalRelationId,
    position: usize,
    replace: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let name = ColumnIdentity::from_parts_with_control(qualifier, column, control)?;
    if !replace && names.contains_key(&name) {
        return Ok(());
    }
    let value = input_expression(relation, position, control)?;
    control.check()?;
    match control.budget() {
        Some(budget) => {
            names.insert_budgeted(name, value, budget)?;
        }
        None => {
            names.insert(name, value);
        }
    }
    Ok(())
}

fn virtual_names(
    names: &mut Names,
    schema: &RowSchema,
    relation: InternalRelationId,
    offset: usize,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for (position, (identity, _)) in schema
        .typed_virtual_identities()
        .filter(|(identity, _)| identity.qualifier().is_some())
        .enumerate()
    {
        insert(
            names,
            identity.qualifier(),
            identity.column(),
            relation,
            offset + position,
            true,
            control,
        )?;
        insert(
            names,
            None,
            identity.column(),
            relation,
            offset + position,
            false,
            control,
        )?;
    }
    Ok(())
}
