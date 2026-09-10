//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind complete table declarations around their sequence and catalog publication boundaries.
use crate::ast::{ColumnDef, ColumnType, CreateTable, Expr};
use crate::schema::constraints::validate_foreign_key_definition;
use crate::schema::foreign_keys::{resolve_foreign_key_parent, ForeignKeyDefinitionContext};
use crate::schema::indexes::names::IndexNameCatalog;
use crate::schema::inheritance::InheritanceContext;
use crate::schema::{SchemaBindingContext, SchemaExpressionCatalog};
use crate::semantics::conflict::InferenceBindingScope;
use crate::type_resolution::FunctionTypeResolver;
use crate::SQLError;

pub struct CreateTableAnalysisContext<'a> {
    pub types: &'a dyn FunctionTypeResolver,
    pub schema: &'a dyn SchemaExpressionCatalog,
    pub bindings: &'a dyn InferenceBindingScope,
    pub inheritance: InheritanceContext<'a>,
    pub index_names: &'a dyn IndexNameCatalog,
    pub foreign_keys: ForeignKeyDefinitionContext<'a>,
}

pub fn prepare_create_table_declaration(
    context: &CreateTableAnalysisContext<'_>,
    c: &mut CreateTable,
) -> Result<(), SQLError> {
    for column in &mut c.columns {
        column.ty =
            crate::type_resolution::resolve_declared_column_type(context.types, &column.ty)?;
    }
    super::super::inheritance::prepare_create_table_hierarchy(&context.inheritance, c)?;
    super::super::indexes::names::name_constraint_indexes(
        context.index_names,
        &c.name,
        &mut c.key_constraints,
    )?;
    bind_create_table_relation_references(context.foreign_keys.catalog, c)?;
    Ok(())
}

pub fn validate_create_table_expressions(
    context: &CreateTableAnalysisContext<'_>,
    c: &mut CreateTable,
) -> Result<(), SQLError> {
    let check_columns = c.columns.clone();
    for column in &mut c.columns {
        if let Some(default) = &mut column.default {
            validate_default_expression(context, default, &column.ty)?;
        }
        if let Some(check) = &mut column.check {
            validate_check_expression(context, &c.name, &c.qualifier, &check_columns, check)?;
            crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
                context.schema,
                check,
                None,
            )?;
        }
    }
    for check in &mut c.checks {
        validate_check_expression(
            context,
            &c.name,
            &c.qualifier,
            &check_columns,
            &mut check.expr,
        )?;
        crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
            context.schema,
            &check.expr,
            None,
        )?;
    }
    super::super::check_inheritance::merge_create_checks(c)?;
    for foreign_key in &mut c.foreign_keys {
        if !foreign_key.period {
            continue;
        }
        let self_reference = foreign_key.ref_table == c.name
            || foreign_key.ref_table == c.qualifier
            || c.name
                .rsplit_once('.')
                .is_some_and(|(_, local_name)| local_name == foreign_key.ref_table);
        if self_reference {
            validate_foreign_key_definition(
                &c.name,
                &c.columns,
                &c.name,
                &c.columns,
                &c.key_constraints,
                foreign_key,
            )?;
            foreign_key.ref_table.clone_from(&c.name);
        } else {
            let (canonical, parent_columns, parent_keys) =
                resolve_foreign_key_parent(&context.foreign_keys, &foreign_key.ref_table)?;
            validate_foreign_key_definition(
                &c.name,
                &c.columns,
                &canonical,
                &parent_columns,
                &parent_keys,
                foreign_key,
            )?;
            foreign_key.ref_table = canonical;
        }
    }
    super::super::generated::prepare_generated_columns(
        context.schema,
        &c.qualifier,
        &mut c.columns,
        &c.key_constraints,
        &c.foreign_keys,
    )?;
    Ok(())
}

pub fn bind_created_table_foreign_keys(
    context: &ForeignKeyDefinitionContext<'_>,
    c: &mut CreateTable,
    registered_columns: &mut [ColumnDef],
) -> Result<(), SQLError> {
    for column in registered_columns {
        let Some(reference) = column.references.clone() else {
            continue;
        };
        let mut foreign_key = super::super::foreign_keys::column_foreign_key(column, &reference);
        super::super::foreign_keys::validate_bound_foreign_key_definition_with_local_state(
            context,
            &c.name,
            None,
            Some(&c.key_constraints),
            &mut foreign_key,
        )?;
        let [referenced_column] = foreign_key.ref_columns.as_slice() else {
            return Err(SQLError::Internal(
                "column FOREIGN KEY did not resolve exactly one referenced column".into(),
            ));
        };
        let Some(reference) = column.references.as_mut() else {
            return Err(SQLError::Internal(
                "column FOREIGN KEY disappeared during validation".into(),
            ));
        };
        reference.referenced_key = foreign_key.referenced_key;
        reference.table = foreign_key.ref_table;
        reference.column = Some(referenced_column.clone());
    }
    for foreign_key in &mut c.foreign_keys {
        super::super::foreign_keys::validate_bound_foreign_key_definition_with_local_state(
            context,
            &c.name,
            None,
            Some(&c.key_constraints),
            foreign_key,
        )?;
    }
    Ok(())
}

fn validate_default_expression(
    context: &CreateTableAnalysisContext<'_>,
    expression: &mut Expr,
    target: &ColumnType,
) -> Result<(), SQLError> {
    let binding = context.bindings.binding_scope()?;
    super::super::defaults::validate_default_expression(
        &SchemaBindingContext {
            catalog: context.schema,
            binding: &binding.context(),
        },
        expression,
        target,
    )
}

fn validate_check_expression(
    context: &CreateTableAnalysisContext<'_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let binding = context.bindings.binding_scope()?;
    super::super::constraints::validate_check_expression(
        &SchemaBindingContext {
            catalog: context.schema,
            binding: &binding.context(),
        },
        table,
        qualifier,
        columns,
        expression,
    )
}

fn bind_create_table_relation_references(
    catalog: &dyn super::super::foreign_keys::ForeignKeyDefinitionCatalog,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    let table_name = table.name.clone();
    let qualifier = table.qualifier.clone();
    for column in &mut table.columns {
        if let Some(reference) = column.references.as_mut() {
            bind_create_table_reference(catalog, &table_name, &qualifier, &mut reference.table)?;
        }
    }
    for foreign_key in &mut table.foreign_keys {
        bind_create_table_reference(catalog, &table_name, &qualifier, &mut foreign_key.ref_table)?;
    }
    Ok(())
}

fn bind_create_table_reference(
    catalog: &dyn super::super::foreign_keys::ForeignKeyDefinitionCatalog,
    table: &str,
    qualifier: &str,
    reference: &mut String,
) -> Result<(), SQLError> {
    let self_reference = reference == table
        || reference == qualifier
        || table
            .rsplit_once('.')
            .is_some_and(|(_, local_name)| local_name == reference);
    if self_reference {
        table.clone_into(reference);
        return Ok(());
    }
    *reference = catalog.resolve_table_reference(reference)?;
    Ok(())
}
