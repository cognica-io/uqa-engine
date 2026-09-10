//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute COPY byte streams through the ordinary INSERT and SELECT statement paths.
use std::io::{Read, Write};
use uqa_core::Value;
use uqa_sql::{
    ast::{Expr, InsertStmt, ReturningAliases, Statement},
    catalog::security::table::TableAclPrivilege,
    copy::stream::CopyCatalog,
    copy::{
        compile_copy, decode_copy_input, encode_copy_result_with_engine, CopyDirection, CopyTarget,
    },
    expr::EngineHook,
    SQLError, SQLParam, SQLResult,
};
pub trait CopyPrivileges {
    fn ensure_any_column(&self, table: &str, privilege: TableAclPrivilege) -> Result<(), SQLError>;
    fn ensure_column(
        &self,
        table: &str,
        column: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
}
pub trait CopyStatements {
    fn execute_statement(
        &self,
        statement: Statement,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError>;
    fn execute_text(&self, text: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError>;
}
pub struct CopyExecutionContext<'a> {
    pub catalog: &'a dyn CopyCatalog,
    pub privileges: &'a dyn CopyPrivileges,
    pub statements: &'a dyn CopyStatements,
    pub output: &'a dyn EngineHook,
}
pub fn copy_from(
    context: &CopyExecutionContext<'_>,
    statement: &str,
    input: &mut impl Read,
) -> Result<u64, SQLError> {
    let copy = compile_copy(statement)?;
    uqa_sql::copy::stream::validate_stream(&copy, CopyDirection::From)?;
    let CopyTarget::Relation {
        name: relation,
        qualifier,
        columns: requested_columns,
    } = &copy.target
    else {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "COPY FROM requires a relation target".into(),
        });
    };
    let (canonical, columns) = uqa_sql::copy::stream::relation_columns(
        context.catalog,
        relation,
        qualifier,
        requested_columns,
        false,
    )?;
    if columns.is_empty() {
        context
            .privileges
            .ensure_any_column(&canonical, TableAclPrivilege::Insert)?;
    } else {
        for column in &columns {
            context
                .privileges
                .ensure_column(&canonical, column, TableAclPrivilege::Insert)?;
        }
    }
    let mut bytes = Vec::new();
    input
        .read_to_end(&mut bytes)
        .map_err(|error| copy_io_error("read COPY FROM stream", error))?;
    let rows = decode_copy_input(&bytes, &copy.options, &columns)?;
    if rows.is_empty() {
        return Ok(0);
    }
    if columns.is_empty() {
        return Err(SQLError::Unsupported(
            "COPY FROM for a zero-column relation is not implemented".into(),
        ));
    }
    let mut params = Vec::with_capacity(rows.len().saturating_mul(columns.len()));
    let mut parameter = 1usize;
    let mut insert_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let mut values = Vec::with_capacity(row.len());
        for field in row {
            values.push(Expr::Param(parameter));
            parameter = parameter.checked_add(1).ok_or_else(|| SQLError::Routine {
                sqlstate: "54000".into(),
                message: "COPY input has too many fields".into(),
            })?;
            params.push(SQLParam::Scalar(match field {
                Some(value) => Value::Str(value),
                None => Value::Null,
            }));
        }
        insert_rows.push(values);
    }
    let result = context.statements.execute_statement(
        Statement::Insert(InsertStmt {
            table: relation.clone(),
            target_relation_bound: false,
            target_qualifier: qualifier.clone(),
            include_descendants: true,
            columns,
            with: Vec::new(),
            rows: insert_rows,
            select_source: None,
            on_conflict: None,
            returning: Vec::new(),
            returning_aliases: ReturningAliases::default(),
        }),
        &params,
    )?;
    Ok(result.affected_rows)
}

pub fn copy_to(
    context: &CopyExecutionContext<'_>,
    statement: &str,
    output: &mut impl Write,
) -> Result<u64, SQLError> {
    let copy = compile_copy(statement)?;
    uqa_sql::copy::stream::validate_stream(&copy, CopyDirection::To)?;
    let query = match &copy.target {
        CopyTarget::Relation {
            name: relation,
            qualifier,
            columns: requested_columns,
        } => {
            let (canonical, columns) = uqa_sql::copy::stream::relation_columns(
                context.catalog,
                relation,
                qualifier,
                requested_columns,
                true,
            )?;
            if columns.is_empty() {
                context
                    .privileges
                    .ensure_any_column(&canonical, TableAclPrivilege::Select)?;
            } else {
                for column in &columns {
                    context.privileges.ensure_column(
                        &canonical,
                        column,
                        TableAclPrivilege::Select,
                    )?;
                }
            }
            let projection = columns
                .iter()
                .map(|column| uqa_sql::expr::quote_ident(column))
                .collect::<Vec<_>>()
                .join(", ");
            format!("SELECT {projection} FROM ONLY {relation}")
        }
        CopyTarget::Query(query) => query.clone(),
    };
    let result = context.statements.execute_text(&query, &[])?;
    let row_count = u64::try_from(result.rows.len()).map_err(|_| SQLError::Routine {
        sqlstate: "54000".into(),
        message: "COPY output row count exceeds u64".into(),
    })?;
    let bytes = encode_copy_result_with_engine(&result, &copy.options, context.output)?;
    output
        .write_all(&bytes)
        .map_err(|error| copy_io_error("write COPY TO stream", error))?;
    Ok(row_count)
}

fn copy_io_error(action: &str, error: std::io::Error) -> SQLError {
    SQLError::Routine {
        sqlstate: "58030".into(),
        message: format!("{action}: {error}"),
    }
}
