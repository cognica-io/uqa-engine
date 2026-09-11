//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute anonymous blocks after SQL-owned language and datum analysis.
use super::{context::AnonymousBlockContext, depth::DepthGuard};
use crate::routines::{
    transaction::{nonatomic_routine_entry_allowed, RoutineTransactionGuard},
    Interpreter,
};
use uqa_sql::{SQLError, SQLResult};
pub fn run_do_block(
    context: &AnonymousBlockContext<'_>,
    language: &str,
    body: &str,
    nested_statement: bool,
) -> Result<SQLResult, SQLError> {
    let (def, parsed) = uqa_sql::routines::anonymous_block::compile_do_block(
        context.types,
        context.parsers,
        language,
        body,
    )?;
    let _guard = DepthGuard::enter(context.session)?;
    let _transaction_context = RoutineTransactionGuard::enter(
        context.runtime.session,
        nonatomic_routine_entry_allowed(context.runtime.session, nested_statement),
    );
    let mut interpreter = Interpreter::new(context.runtime, &def, &parsed, Vec::new())?;
    interpreter.run(&parsed.action)?;
    Ok(SQLResult::empty())
}
