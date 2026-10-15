//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A physical relation supplies its live CHECK row and tableoid without copying its document.

use uqa_core::Value;
use uqa_sql::{
    ast::{ColumnDef, Expr},
    expr::RowLookup,
    semantics::partition::PartitionExpressions,
    ColumnType, FunctionTypeResolver, ResultRow, RowSchema, SQLError, SQLParam,
};

pub(crate) struct CheckRowScope {
    qualifier: String,
    schema: RowSchema,
    tableoid: Value,
}

impl CheckRowScope {
    pub(crate) fn new(
        types: &dyn FunctionTypeResolver,
        table: &str,
        columns: &[ColumnDef],
    ) -> Result<Self, SQLError> {
        let ty = types
            .resolve_type_name(table)?
            .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
        let ColumnType::Composite(reference) = &ty else {
            return Err(SQLError::Internal(
                "a CHECK relation has no named row type".into(),
            ));
        };
        Ok(Self {
            qualifier: reference.name.clone(),
            schema: uqa_sql::schema::constraints::check_row_schema(&reference.name, columns, &ty)?,
            tableoid: Value::Int(i64::from(reference.relation_oid)),
        })
    }

    pub(crate) fn evaluate(
        &self,
        expressions: &dyn PartitionExpressions,
        expression: &Expr,
        row: &ResultRow,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        expressions.evaluate_row(
            expression,
            &CheckRow { scope: self, row },
            &self.schema,
            params,
        )
    }
}

struct CheckRow<'a> {
    scope: &'a CheckRowScope,
    row: &'a ResultRow,
}

impl RowLookup for CheckRow<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        if name == "tableoid" {
            Some(&self.scope.tableoid)
        } else {
            self.scope
                .schema
                .column_slot(name)
                .map(|_| self.row.get(name).unwrap_or(&Value::Null))
        }
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value> {
        (qualifier == self.scope.qualifier)
            .then(|| self.column(column))
            .flatten()
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        self.row.visit_columns(visitor);
    }
}

#[cfg(test)]
mod tests;
