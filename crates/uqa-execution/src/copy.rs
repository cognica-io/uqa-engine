//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute COPY byte streams through the ordinary INSERT and SELECT statement paths.
use crate::{query::output::QueryRows, statement::plan_executor::StatementOutput};
use std::io::{Read, Write};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};
use uqa_sql::{
    ast::{Expr, InsertStmt, ReturningAliases, Statement},
    catalog::security::table::TableAclPrivilege,
    copy::stream::CopyCatalog,
    copy::{
        compile_copy, decode_copy_input, write_copy_result, CopyDirection, CopyEncoder, CopyTarget,
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
    fn execute_text_with_output(
        &self,
        text: &str,
        params: &[SQLParam],
        consume: &mut dyn FnMut(StatementOutput) -> Result<(), SQLError>,
    ) -> Result<(), SQLError>;
}
pub struct CopyExecutionContext<'a> {
    pub catalog: &'a dyn CopyCatalog,
    pub privileges: &'a dyn CopyPrivileges,
    pub statements: &'a dyn CopyStatements,
    pub output: &'a dyn EngineHook,
    pub memory: &'a dyn crate::query::runtime::QueryMemorySettings,
    pub cancellation: &'a CancellationToken,
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
            target_alias: None,
            include_descendants: true,
            columns: columns.into_iter().map(Into::into).collect(),
            // COPY FROM writes the values its input supplies, also to a GENERATED ALWAYS identity column.
            overriding: Some(uqa_sql::ast::OverridingKind::SystemValue),
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
    let mut row_count = 0;
    context
        .statements
        .execute_text_with_output(&query, &[], &mut |result| {
            let memory = MemoryBudget::new(context.memory.work_mem_bytes()?);
            let control =
                ProductionControl::new(&memory, context.cancellation, context.cancellation);
            row_count = write_statement_output(
                result,
                &copy.options,
                output,
                Some(context.output),
                &control,
            )?;
            Ok(())
        })?;
    Ok(row_count)
}

pub(crate) fn write_statement_output(
    result: StatementOutput,
    options: &uqa_sql::copy::CopyOptions,
    output: &mut impl Write,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<u64, SQLError> {
    match result {
        StatementOutput::Command(result) => {
            let count = copy_row_count(result.rows.len())?;
            write_copy_result(&result, options, output, engine, control)?;
            Ok(count)
        }
        StatementOutput::Mutation(result) => {
            let count = copy_row_count(result.row_count())?;
            let schema = result.schema.clone();
            write_copy_batches(
                schema.columns(),
                schema.column_types(),
                result.into_batches(),
                options,
                output,
                engine,
                control,
            )?;
            Ok(count)
        }
        StatementOutput::Query(result) => {
            let QueryRows::SharedSpill(rows) = result.rows else {
                return Err(SQLError::Internal(
                    "COPY query returned unbounded rows".into(),
                ));
            };
            let count = copy_row_count(rows.rows())?;
            let rows = rows
                .into_reader()
                .map_err(crate::physical::physical_exec_error)?;
            write_copy_batches(
                &result.columns,
                &result.column_types,
                rows.map(|batch| batch.map_err(crate::physical::physical_exec_error)),
                options,
                output,
                engine,
                control,
            )?;
            Ok(count)
        }
    }
}

fn write_copy_batches(
    columns: &[String],
    column_types: &[Option<uqa_sql::ast::ColumnType>],
    mut batches: impl Iterator<Item = Result<crate::Batch, SQLError>>,
    options: &uqa_sql::copy::CopyOptions,
    output: &mut impl Write,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut encoder = CopyEncoder::new(options, output, engine, *control)?;
    encoder.write_header(columns)?;
    loop {
        control.check()?;
        let Some(batch) = batches.next() else { break };
        let batch = batch?;
        for row in batch.rows {
            let row = crate::OwnedPhysicalRow::new(batch.schema.clone(), row);
            let view = row.view();
            encoder.write_row((0..columns.len()).map(|column| {
                (
                    view.value_at(column),
                    column_types.get(column).and_then(Option::as_ref),
                )
            }))?;
        }
    }
    encoder.finish()?;
    Ok(())
}

fn copy_row_count(rows: usize) -> Result<u64, SQLError> {
    u64::try_from(rows).map_err(|_| SQLError::Routine {
        sqlstate: "54000".into(),
        message: "COPY output row count exceeds u64".into(),
    })
}

fn copy_io_error(action: &str, error: std::io::Error) -> SQLError {
    SQLError::Routine {
        sqlstate: "58030".into(),
        message: format!("{action}: {error}"),
    }
}

#[cfg(test)]
mod tests;
