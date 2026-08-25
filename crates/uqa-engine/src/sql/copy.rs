//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming `COPY FROM STDIN` and `COPY TO STDOUT` execution.

use std::collections::BTreeSet;
use std::io::{Read, Write};

use uqa_core::Value;
use uqa_sql::ast::{Expr, InsertStmt, ReturningAliases, Statement};
use uqa_sql::copy::{
    compile_copy, decode_copy_input, encode_copy_result, CopyDirection, CopyEndpoint, CopyFormat,
    CopyStatement, CopyTarget,
};
use uqa_sql::{SQLError, SQLParam};

use super::Engine;

impl Engine {
    /// Consume a `PostgreSQL` text or CSV `COPY relation FROM STDIN` stream.
    ///
    /// The complete input is decoded before the single underlying multi-row
    /// insert starts. Row routing, defaults, identity allocation, generated
    /// columns, checks, foreign keys, and statement rollback consequently use
    /// exactly the same implementation as `INSERT`.
    pub fn copy_from(&self, statement: &str, mut input: impl Read) -> Result<u64, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let result = self.copy_from_inner(statement, &mut input);
        result.map_err(|error| self.abort_sql_transaction_after_error(error))
    }

    fn copy_from_inner(&self, statement: &str, input: &mut impl Read) -> Result<u64, SQLError> {
        self.synchronize_for_copy()?;
        let copy = compile_copy(statement)?;
        ensure_copy_direction(&copy, CopyDirection::From)?;
        ensure_stdio_endpoint(&copy)?;
        ensure_stream_format(&copy)?;
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
        let columns = self.copy_relation_columns(relation, qualifier, requested_columns, false)?;
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
        let result = super::execute_compiled_statement(
            self,
            Statement::Insert(InsertStmt {
                table: relation.clone(),
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

    /// Write a `PostgreSQL` text or CSV `COPY ... TO STDOUT` stream and return
    /// the number of emitted rows.
    pub fn copy_to(&self, statement: &str, mut output: impl Write) -> Result<u64, SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let result = self.copy_to_inner(statement, &mut output);
        result.map_err(|error| self.abort_sql_transaction_after_error(error))
    }

    fn copy_to_inner(&self, statement: &str, output: &mut impl Write) -> Result<u64, SQLError> {
        self.synchronize_for_copy()?;
        let copy = compile_copy(statement)?;
        ensure_copy_direction(&copy, CopyDirection::To)?;
        ensure_stdio_endpoint(&copy)?;
        ensure_stream_format(&copy)?;
        let query = match &copy.target {
            CopyTarget::Relation {
                name: relation,
                qualifier,
                columns: requested_columns,
            } => {
                let columns =
                    self.copy_relation_columns(relation, qualifier, requested_columns, true)?;
                let projection = columns
                    .iter()
                    .map(|column| uqa_sql::expr::quote_ident(column))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("SELECT {projection} FROM ONLY {relation}")
            }
            CopyTarget::Query(query) => query.clone(),
        };
        let result = super::execute(self, &query, &[])?;
        let row_count = u64::try_from(result.rows.len()).map_err(|_| SQLError::Routine {
            sqlstate: "54000".into(),
            message: "COPY output row count exceeds u64".into(),
        })?;
        let bytes = encode_copy_result(&result, &copy.options)?;
        output
            .write_all(&bytes)
            .map_err(|error| copy_io_error("write COPY TO stream", error))?;
        Ok(row_count)
    }

    fn synchronize_for_copy(&self) -> Result<(), SQLError> {
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("refresh table catalog: {error}")))?;
        self.synchronize_table_data().map_err(|error| {
            SQLError::Internal(format!("refresh committed table data: {error}"))
        })?;
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("refresh durable catalog registries: {error}"))
        })
    }

    fn copy_relation_columns(
        &self,
        relation: &str,
        display_name: &str,
        requested: &[String],
        reject_partitioned_output: bool,
    ) -> Result<Vec<String>, SQLError> {
        let canonical = self
            .try_resolve_table_name(relation)
            .map_err(|error| {
                SQLError::Internal(format!("resolve COPY relation `{relation}`: {error}"))
            })?
            .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
        let table = self
            .try_table(&canonical)
            .map_err(|error| {
                SQLError::Internal(format!("read COPY relation `{canonical}`: {error}"))
            })?
            .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
        let definitions = table.columns.read().clone();
        let columns = if requested.is_empty() {
            definitions
                .into_iter()
                .filter(|column| column.generated.is_none())
                .map(|column| column.name)
                .collect()
        } else {
            let mut seen = BTreeSet::new();
            let mut columns = Vec::with_capacity(requested.len());
            for requested in requested {
                if !seen.insert(requested.clone()) {
                    return Err(SQLError::Routine {
                        sqlstate: "42701".into(),
                        message: format!("column \"{requested}\" specified more than once"),
                    });
                }
                let Some(column) = definitions.iter().find(|column| column.name == *requested)
                else {
                    return Err(SQLError::Routine {
                        sqlstate: "42703".into(),
                        message: format!(
                            "column \"{requested}\" of relation \"{display_name}\" does not exist"
                        ),
                    });
                };
                if column.generated.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "42P10".into(),
                        message: format!(
                            "column \"{requested}\" is a generated column\nDETAIL: Generated columns cannot be used in COPY."
                        ),
                    });
                }
                columns.push(requested.clone());
            }
            columns
        };
        if reject_partitioned_output && table.hierarchy.read().partition_spec.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!(
                    "cannot copy from partitioned table \"{display_name}\"\nHINT: Try the COPY (SELECT ...) TO variant."
                ),
            });
        }
        Ok(columns)
    }
}

fn ensure_copy_direction(copy: &CopyStatement, expected: CopyDirection) -> Result<(), SQLError> {
    if copy.direction == expected {
        return Ok(());
    }
    let expected = match expected {
        CopyDirection::From => "FROM STDIN",
        CopyDirection::To => "TO STDOUT",
    };
    Err(SQLError::Routine {
        sqlstate: "42601".into(),
        message: format!("COPY stream API requires COPY {expected}"),
    })
}

fn ensure_stdio_endpoint(copy: &CopyStatement) -> Result<(), SQLError> {
    match &copy.endpoint {
        CopyEndpoint::Stdio => Ok(()),
        CopyEndpoint::File(_) => Err(SQLError::Unsupported(
            "server-side COPY files are not available through the embedded stream API".into(),
        )),
        CopyEndpoint::Program(_) => Err(SQLError::Unsupported(
            "COPY PROGRAM is not available through the embedded stream API".into(),
        )),
    }
}

fn ensure_stream_format(copy: &CopyStatement) -> Result<(), SQLError> {
    if copy.options.format == CopyFormat::Binary {
        Err(SQLError::Unsupported(
            "binary COPY format is not implemented".into(),
        ))
    } else {
        Ok(())
    }
}

fn copy_io_error(action: &str, error: std::io::Error) -> SQLError {
    SQLError::Routine {
        sqlstate: "58030".into(),
        message: format!("{action}: {error}"),
    }
}
