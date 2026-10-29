//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed SQL set-function bodies eligible to become a caller-owned subquery.

use super::{CreateFunction, FunctionBinding, RoutineInliningContext, SQLError, ScalarExpr};
use crate::ast::FunctionVolatility;
use crate::plan::QueryPlan;

mod owned;
mod production;
mod relations;
mod scopes_owned;
pub use owned::InlineOwnedRoutineQuery;

pub struct InlineRoutineQuery {
    pub identity: [u8; 16],
    pub query: Box<QueryPlan>,
    /// Declared OUT names or composite attributes; a scalar result is named by its FROM alias.
    pub declared_columns: Option<Vec<String>>,
}

impl RoutineInliningContext<'_> {
    /// Describe a selected scalar FROM call without evaluating its body or arguments.
    pub fn scalar_from_columns(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        output_name: &str,
        alias: Option<&str>,
        columns: &[String],
        ordinality: bool,
    ) -> Result<Option<Vec<String>>, SQLError> {
        self.scalar_from_columns_with_control(
            binding,
            arguments,
            output_name,
            alias,
            columns,
            ordinality,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .map(|columns| {
            columns.map(|columns| {
                columns
                    .into_uncontrolled()
                    .expect("ordinary scalar source columns")
            })
        })
    }

    /// Expand an ordinary SQL set function under the caller's snapshot. Volatile inputs, subplans, security boundaries and ordinality retain the function scan.
    pub fn prepare_table(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        active: &[[u8; 16]],
        column_names: &[String],
        column_types: &[String],
    ) -> Result<Option<InlineRoutineQuery>, SQLError> {
        let control = uqa_core::memory::ProductionControl::uncontrolled();
        self.prepare_table_owned(
            binding,
            arguments,
            active,
            column_names,
            column_types,
            &control,
        )?
        .map(|expanded| {
            Ok(InlineRoutineQuery {
                identity: expanded.identity,
                query: Box::new(
                    expanded
                        .query
                        .finish(&control)?
                        .into_uncontrolled()
                        .expect("ordinary inline table body"),
                ),
                declared_columns: expanded.declared_columns.map(|columns| {
                    columns
                        .into_uncontrolled()
                        .expect("ordinary declared columns")
                }),
            })
        })
        .transpose()
    }
}

pub(super) fn eligible_definition(definition: &CreateFunction) -> bool {
    definition.language == "sql"
        && definition.returns_set()
        && definition.volatility != FunctionVolatility::Volatile
        && !definition.strict
        && !definition.security.security_definer
        && definition.config.is_empty()
}
