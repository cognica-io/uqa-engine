//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declare SQL and PL/pgSQL portals against their current statement scope.

use super::context::{
    PortalExecutionContext, SessionPortalCommandDeclaration, SessionPortalDeclaration,
};
use crate::{
    query::{
        binding::analyze_query_plan_schema,
        locking::{lock_query_relations, query_has_row_locks, validate_query_row_locks},
    },
    statement::context::session::StatementPortals,
};
use uqa_sql::{
    plan::{CommandPlan, QueryPlan, UnifiedPlan},
    routines::resolution::RoutineOverloadContext,
    semantics::portals::{
        cannot_open_command_cursor, command_scroll_returns_nulls, validate_query_options,
        PortalDeclarationContext,
    },
    SQLError, SQLParam, SQLResult,
};

pub fn declare_session_portal<S: Clone + Send + Sync + 'static>(
    inputs: &PortalExecutionContext<'_, S>,
    params: &[SQLParam],
    name: &str,
    binary: bool,
    scroll: Option<bool>,
    hold: bool,
    query: &QueryPlan,
) -> Result<SQLResult, SQLError> {
    prepare_session_portal(
        inputs,
        params,
        name,
        binary,
        scroll,
        hold,
        query,
        PortalDeclarationContext::Sql,
    )?;
    Ok(SQLResult::empty())
}

pub fn open_plpgsql_session_portal<S: Clone + Send + Sync + 'static>(
    inputs: &PortalExecutionContext<'_, S>,
    params: &[SQLParam],
    name: &str,
    scroll: Option<bool>,
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => prepare_session_portal(
            inputs,
            params,
            name,
            false,
            scroll,
            false,
            query,
            PortalDeclarationContext::PLpgSQL,
        ),
        UnifiedPlan::Command(command) => {
            open_plpgsql_command_portal(inputs, params, name, scroll, command)
        }
    }
}

fn open_plpgsql_command_portal<S: Clone + Send + Sync + 'static>(
    inputs: &PortalExecutionContext<'_, S>,
    params: &[SQLParam],
    name: &str,
    scroll: Option<bool>,
    command: &CommandPlan,
) -> Result<(), SQLError> {
    let schema = match command {
        CommandPlan::Insert(_)
        | CommandPlan::Update(_)
        | CommandPlan::Delete(_)
        | CommandPlan::Merge(_) => cursor_command_returning_schema(inputs, command, params)?,
        CommandPlan::Call { name, args } => analyze_call_result_schema(inputs, name, args, params)?,
        CommandPlan::ShowVariable { name } => {
            inputs.session.show_variable(name)?;
            Some(crate::RowSchema::with_types(
                vec![name.clone()],
                vec![Some(uqa_sql::ColumnType::Text)],
            ))
        }
        CommandPlan::Explain { body, format, .. } => {
            validate_explain_cursor_body(inputs, params, body)?;
            let result = (inputs.explain)(body, false, format.as_deref(), None)?;
            Some(crate::RowSchema::with_types(
                result.columns,
                result.column_types,
            ))
        }
        _ => None,
    }
    .ok_or_else(|| cannot_open_command_cursor(command))?;
    let null_returning_values = command_scroll_returns_nulls(command, scroll)?;
    inputs
        .state
        .open_pending_command_session_portal(SessionPortalCommandDeclaration {
            name: name.to_string(),
            command: Box::new(command.clone()),
            params: params.to_vec(),
            columns: schema.columns().to_vec(),
            column_types: schema.column_types().to_vec(),
            scrollable: scroll.unwrap_or(false),
            null_returning_values,
        })
}

fn validate_explain_cursor_body<S: Clone + Send + Sync + 'static>(
    inputs: &PortalExecutionContext<'_, S>,
    params: &[SQLParam],
    body: &UnifiedPlan,
) -> Result<(), SQLError> {
    match body {
        UnifiedPlan::Query(query) => {
            lock_query_relations(inputs.queries.row_lock_context(), query)?;
            let ctes = inputs.queries.statement_scope(None);
            analyze_query_plan_schema(inputs.routines, query, params, &ctes, None)?;
            Ok(())
        }
        UnifiedPlan::Command(command) => {
            let _ = cursor_command_returning_schema(inputs, command, params)?;
            Ok(())
        }
    }
}

pub fn ensure_plpgsql_session_portal_available(
    state: &dyn StatementPortals,
    name: &str,
) -> Result<(), SQLError> {
    state
        .ensure_session_portal_available(name)
        .map_err(|error| {
            if error.sqlstate() == Some("42P03") {
                SQLError::Routine {
                    sqlstate: "42P03".into(),
                    message: format!("cursor \"{name}\" already in use"),
                }
            } else {
                error
            }
        })
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SQL and PL/pgSQL portal contracts explicit"
)]
fn prepare_session_portal<S: Clone + Send + Sync + 'static>(
    inputs: &PortalExecutionContext<'_, S>,
    params: &[SQLParam],
    name: &str,
    binary: bool,
    scroll: Option<bool>,
    hold: bool,
    query: &QueryPlan,
    context: PortalDeclarationContext,
) -> Result<(), SQLError> {
    if context == PortalDeclarationContext::Sql && !hold && !inputs.state.in_transaction_block() {
        return Err(SQLError::Routine {
            sqlstate: "25P01".into(),
            message: "DECLARE CURSOR can only be used in transaction blocks".into(),
        });
    }
    if context == PortalDeclarationContext::PLpgSQL {
        ensure_plpgsql_session_portal_available(inputs.state, name)?;
    } else {
        inputs.state.ensure_session_portal_available(name)?;
    }
    let has_row_locks = query_has_row_locks(query);
    validate_query_options(context, has_row_locks, hold, scroll)?;
    lock_query_relations(inputs.queries.row_lock_context(), query)?;
    let ctes = inputs.queries.statement_scope(None);
    let schema = analyze_query_plan_schema(inputs.routines, query, params, &ctes, None)?;
    validate_query_row_locks(inputs.queries.row_lock_context(), query, params)?;
    inputs
        .state
        .open_pending_session_portal(SessionPortalDeclaration {
            name: name.to_string(),
            query: query.clone(),
            params: params.to_vec(),
            columns: schema.columns().to_vec(),
            column_types: schema.column_types().to_vec(),
            scrollable: scroll.unwrap_or(!has_row_locks),
            holdable: hold,
            binary,
        })?;
    Ok(())
}

fn cursor_command_returning_schema<S: Clone + 'static>(
    inputs: &PortalExecutionContext<'_, S>,
    command: &CommandPlan,
    params: &[SQLParam],
) -> Result<Option<crate::RowSchema>, SQLError> {
    crate::mutation::entry::cursor_command_returning_schema(
        &inputs.returning.returning_execution_context(),
        inputs.returning.returning_analysis_context(),
        inputs.command_scopes,
        command,
        params,
    )
}

fn analyze_call_result_schema<S: Clone + 'static>(
    inputs: &PortalExecutionContext<'_, S>,
    name: &str,
    arguments: &[uqa_sql::plan::ExpressionPlan],
    params: &[uqa_sql::SQLParam],
) -> Result<Option<uqa_sql::RowSchema>, SQLError> {
    let analysis = uqa_sql::routines::call::ProcedureCallAnalysis::new(arguments)?;
    let scope = inputs.queries.statement_scope(None);
    analysis.result_schema(
        name,
        &RoutineOverloadContext {
            catalog: inputs.overloads,
        },
        inputs.types,
        &mut |argument| {
            crate::query::binding::bind_expression_plan_type(
                inputs.routines,
                argument,
                params,
                &scope,
            )
        },
    )
}
