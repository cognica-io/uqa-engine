//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable table-call inputs shared by separately evaluated outer rows.

use super::{FunctionRows, TableFunctionCall};
use crate::query::set_projection::SetFunctionRuntime;
use crate::scalar::PreparedExpressions;
use std::sync::Arc;
use uqa_sql::{
    ast::FunctionBinding, routines::invocation::descriptions::RoutineCallDescriptions, SQLParam,
    ScalarExpr,
};

pub(super) struct PreparedFunctionCall {
    name: String,
    binding: Option<FunctionBinding>,
    output_name: String,
    arguments: PreparedExpressions<Vec<ScalarExpr>>,
    arguments_bound: bool,
    descriptions: RoutineCallDescriptions,
    alias: Option<String>,
    column_aliases: Vec<String>,
    ordinality: bool,
    column_types: Vec<String>,
}

impl PreparedFunctionCall {
    pub(super) fn new(call: &TableFunctionCall<'_>, bound: Option<Vec<ScalarExpr>>) -> Self {
        Self {
            name: call.name.to_owned(),
            binding: call.binding.cloned(),
            output_name: call.output_name.to_owned(),
            arguments_bound: bound.is_some(),
            arguments: PreparedExpressions::scalars(bound.unwrap_or_else(|| call.args.to_vec())),
            descriptions: RoutineCallDescriptions::default(),
            alias: call.alias.map(str::to_owned),
            column_aliases: call.column_aliases.to_vec(),
            ordinality: call.ordinality,
            column_types: call.column_types.to_vec(),
        }
    }

    fn call(&self) -> TableFunctionCall<'_> {
        TableFunctionCall {
            name: &self.name,
            binding: self.binding.as_ref(),
            output_name: &self.output_name,
            relations: None,
            args: &self.arguments,
            function_states: self.arguments_bound.then(|| self.arguments.calls()),
            routine_descriptions: Some(&self.descriptions),
            alias: self.alias.as_deref(),
            column_aliases: &self.column_aliases,
            ordinality: self.ordinality,
            column_types: &self.column_types,
        }
    }
}

/// Each stream owns only its current row and a shared immutable call. No argument or body is evaluated until the first pull, and an unfetched stream has no effects.
pub(super) fn deferred_function_rows<'a>(
    call: Arc<PreparedFunctionCall>,
    runtime: Arc<dyn SetFunctionRuntime + 'a>,
    params: &'a [SQLParam],
    outer_row: Option<crate::OwnedPhysicalRow>,
    width: usize,
) -> FunctionRows<'a> {
    crate::SQLRowStream::from_source(
        crate::query::table_functions::DeferredTableFunctionRows::new(move || {
            runtime
                .table_function_rows(call.call(), params, outer_row.as_ref())?
                .into_rows(width, &format!("table function `{}`", call.name))
        }),
    )
}
