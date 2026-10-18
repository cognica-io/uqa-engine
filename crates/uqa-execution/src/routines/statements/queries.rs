//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedded SQL results and diagnostics, kept off the recursive statement dispatcher stack.

use crate::routines::{
    result_row_count, result_row_values, return_query_context_error, strict_into_check, Flow,
    Interpreter, IntoTarget, PLpgSQLExpression, PLpgSQLStatement, SQLError, Value,
};

impl Interpreter<'_> {
    pub(super) fn exec_return_query(&mut self, query: &PLpgSQLStatement) -> Result<Flow, SQLError> {
        if !self.is_set {
            return Err(return_query_context_error());
        }
        let result = self.exec_query(query)?;
        self.append_query_rows(&result)?;
        // PostgreSQL sets ROW_COUNT (but not FOUND) here.
        self.last_row_count = result_row_count(&result)?;
        Ok(Flow::Normal)
    }

    pub(super) fn exec_return_query_execute(
        &mut self,
        query: &PLpgSQLExpression,
        params: &[PLpgSQLExpression],
    ) -> Result<Flow, SQLError> {
        if !self.is_set {
            return Err(return_query_context_error());
        }
        let result = self.exec_dynamic(query, params)?;
        self.append_query_rows(&result)?;
        self.last_row_count = result_row_count(&result)?;
        Ok(Flow::Normal)
    }

    pub(super) fn exec_sql_statement(
        &mut self,
        statement: &PLpgSQLStatement,
        into: Option<&IntoTarget>,
        strict: bool,
    ) -> Result<Flow, SQLError> {
        let prepared = self.prepare_statement(statement)?;
        let result = self.execute_fragment(&prepared)?;
        self.consume_statement_result(prepared.is_call(), &result, into, strict)?;
        Ok(Flow::Normal)
    }

    pub(super) fn exec_perform(&mut self, query: &PLpgSQLStatement) -> Result<Flow, SQLError> {
        let result = self.exec_query(query)?;
        let row_count = result_row_count(&result)?;
        self.last_row_count = row_count;
        self.set_found(row_count > 0);
        Ok(Flow::Normal)
    }

    pub(super) fn exec_get_diagnostics(
        &mut self,
        items: &[(String, usize)],
    ) -> Result<Flow, SQLError> {
        for (kind, target) in items {
            match kind.as_str() {
                "ROW_COUNT" => {
                    let count = Value::Int(self.last_row_count);
                    self.assign_datum(*target, count)?;
                }
                other => {
                    return Err(SQLError::Unsupported(format!("GET DIAGNOSTICS {other}")));
                }
            }
        }
        Ok(Flow::Normal)
    }

    pub(super) fn exec_dynamic_into(
        &mut self,
        query: &PLpgSQLExpression,
        params: &[PLpgSQLExpression],
        into: Option<&IntoTarget>,
        strict: bool,
    ) -> Result<Flow, SQLError> {
        let result = self.exec_dynamic(query, params)?;
        let row_count = result_row_count(&result)?;
        self.last_row_count = row_count;
        if let Some(target) = into {
            if strict {
                strict_into_check(row_count)?;
            }
            let values = result_row_values(&result, 0);
            self.assign_into(target, &result.row_schema(), values.as_deref())?;
        }
        // PostgreSQL: EXECUTE never changes FOUND.
        Ok(Flow::Normal)
    }
}
