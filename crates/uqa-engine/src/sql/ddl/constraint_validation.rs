//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared CREATE/ALTER validation for table constraints.

use super::{ColumnType, Engine, SQLError};
use uqa_core::Value;
use uqa_sql::ast::{ColumnDef, Expr, ForeignKey, TableKeyConstraint};
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};

struct CheckConditionTypeResolver<'a> {
    table: &'a str,
    qualifier: &'a str,
    columns: &'a [ColumnDef],
}

impl CheckConditionTypeResolver<'_> {
    fn column(&self, name: &str) -> Result<ResolvedVariable, SQLError> {
        let definition = self
            .columns
            .iter()
            .find(|column| column.name == name)
            .ok_or_else(|| SQLError::UnknownColumn(name.to_string()))?;
        Ok(ResolvedVariable {
            value: Value::Null,
            declared_type: Some(definition.ty.sql_name()),
        })
    }

    fn qualifier_matches(&self, qualifier: &str) -> bool {
        qualifier == self.qualifier
            || qualifier == self.table
            || self
                .table
                .rsplit_once('.')
                .is_some_and(|(_, local)| qualifier == local)
    }
}

impl VariableResolver for CheckConditionTypeResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        self.column(name).map(Some)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if !self.qualifier_matches(qualifier) {
            return Err(SQLError::UnknownTable(qualifier.to_string()));
        }
        self.column(column).map(Some)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

fn is_boolean_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Boolean => true,
        ColumnType::Domain { base, .. } => is_boolean_type(base),
        _ => false,
    }
}

pub(crate) fn validate_check_expression(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let bound = bind_expr(
        expression,
        &mut CheckConditionTypeResolver {
            table,
            qualifier,
            columns,
        },
    )?;
    let lowered = uqa_planner::ExpressionPlan::lower(bound);
    if !lowered.subqueries.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot use subquery in check constraint".into(),
        });
    }
    match uqa_execution::common_context_expression_type(
        &lowered.scalar,
        &uqa_execution::RowSchema::default(),
        &[],
        Some(engine),
    )? {
        Some(ty) if !is_boolean_type(&ty) => Err(SQLError::TypeMismatch(format!(
            "argument of CHECK must be type boolean, not type {}",
            ty.sql_name()
        ))),
        None => {
            if let Expr::Literal(value @ (Value::Str(_) | Value::FixedChar(_))) = expression {
                *value = uqa_sql::expr::cast_value(value, "boolean")?;
            } else {
                *expression = Expr::Cast {
                    expr: Box::new(expression.clone()),
                    ty: "boolean".into(),
                };
            }
            Ok(())
        }
        Some(_) => Ok(()),
    }?;
    bind_stored_check_expression_routines(engine, table, qualifier, columns, expression)?;
    Ok(())
}

pub(crate) fn bind_stored_check_expression_routines(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<bool, SQLError> {
    let typed_expression = bind_expr(
        expression,
        &mut CheckConditionTypeResolver {
            table,
            qualifier,
            columns,
        },
    )?;
    super::defaults::bind_stored_schema_expression_routines(engine, expression, typed_expression)
}

pub(super) fn validate_foreign_key_definition(
    local_table: &str,
    local_columns: &[ColumnDef],
    parent_table: &str,
    parent_columns: &[ColumnDef],
    parent_keys: &[TableKeyConstraint],
    foreign_key: &ForeignKey,
) -> Result<(), SQLError> {
    if foreign_key.local_columns.is_empty()
        || foreign_key.local_columns.len() != foreign_key.ref_columns.len()
    {
        return Err(invalid_foreign_key(format!(
            "foreign key on relation \"{local_table}\" has mismatched local and referenced columns"
        )));
    }

    let local_types = foreign_key
        .local_columns
        .iter()
        .map(|name| {
            local_columns
                .iter()
                .find(|column| column.name == *name)
                .map(|column| &column.ty)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{local_table}.{name}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let parent_types = foreign_key
        .ref_columns
        .iter()
        .map(|name| {
            parent_columns
                .iter()
                .find(|column| column.name == *name)
                .map(|column| &column.ty)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{parent_table}.{name}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let target_key = parent_keys.iter().find(|constraint| {
        constraint.columns == foreign_key.ref_columns && constraint.without_overlaps
    });
    if foreign_key.period && target_key.is_none() {
        return Err(invalid_foreign_key(format!(
            "there is no primary key or unique constraint declared WITH WITHOUT OVERLAPS matching the referenced columns for table \"{parent_table}\""
        )));
    }

    if foreign_key.period {
        if foreign_key.local_columns.len() < 2 {
            return Err(invalid_foreign_key(
                "PERIOD foreign key must contain at least one ordinary column and one period column",
            ));
        }
        let local_period = local_types.last().expect("non-empty foreign key");
        let parent_period = parent_types.last().expect("non-empty foreign key");
        if !matches!(
            local_period,
            ColumnType::Range(_) | ColumnType::Multirange(_)
        ) || local_period != parent_period
        {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "PERIOD columns \"{}\" and \"{}\" have incompatible types {} and {}",
                    foreign_key
                        .local_columns
                        .last()
                        .expect("non-empty foreign key"),
                    foreign_key
                        .ref_columns
                        .last()
                        .expect("non-empty foreign key"),
                    local_period.sql_name(),
                    parent_period.sql_name()
                ),
            });
        }
    }

    Ok(())
}

pub(super) fn resolve_foreign_key_parent(
    engine: &Engine,
    reference: &str,
) -> Result<(String, Vec<ColumnDef>, Vec<TableKeyConstraint>), SQLError> {
    let canonical = engine
        .try_resolve_bound_table_name(reference)?
        .ok_or_else(|| SQLError::UnknownTable(reference.to_string()))?;
    let columns = engine
        .try_describe_table(&canonical)
        .map_err(|error| SQLError::Internal(format!("describe FOREIGN KEY target: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(canonical.clone()))?;
    let keys = engine
        .referenceable_keys(&canonical)
        .map_err(|error| SQLError::Internal(format!("read FOREIGN KEY target keys: {error}")))?;
    Ok((canonical, columns, keys))
}

fn invalid_foreign_key(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42830".into(),
        message: message.into(),
    }
}
