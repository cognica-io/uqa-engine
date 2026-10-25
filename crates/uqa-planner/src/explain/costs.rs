//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explain the native cost model using SQL-owned output types without evaluating rows.

use crate::statement_planning::{estimate_plan_nodes_with_control, StatementStatisticsContext};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    binding::statements::StatementAnalysisContext,
    plan::UnifiedPlan,
    result::explain::{ExplainNodeEstimate, ExplainPlanEstimate},
    ColumnType, SQLError, SQLParam,
};

mod nodes;

pub fn estimate_plan(
    statistics: StatementStatisticsContext<'_>,
    analysis: &StatementAnalysisContext<'_>,
    body: &UnifiedPlan,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ExplainNodeEstimate>>, SQLError> {
    control.check()?;
    let tree = estimate_plan_nodes_with_control(statistics, body, control)?;
    let mut width = None;
    analysis.scopes.with_scope(&mut |scope| {
        control.check()?;
        let binding = scope.binding_context()?;
        let schema = match body {
            UnifiedPlan::Query(query) => Some(uqa_sql::binding::analyze_query_plan_schema(
                analysis.routines,
                query,
                params,
                &binding,
                None,
            )?),
            UnifiedPlan::Command(command) => uqa_sql::binding::analyze_prepared_command_schema(
                analysis.routines,
                command,
                params,
                &binding,
            )?,
        };
        width = Some(
            schema
                .as_ref()
                .map_or(Ok(0), |schema| row_width(schema, control))?,
        );
        Ok(())
    })?;
    let root_width =
        width.ok_or_else(|| SQLError::Internal("EXPLAIN result analysis did not run".into()))?;
    let mut output = ProductionVec::new(*control);
    for (index, node) in tree.nodes.iter().enumerate() {
        let width = if index == 0 {
            Some(root_width)
        } else {
            nodes::output_schema(node.input)
                .map(|schema| row_width(schema, control))
                .transpose()?
        };
        output.push_produced(nodes::capture(node, width, *control)?)?;
    }
    Ok(output.finish()?)
}

fn row_width(
    schema: &uqa_sql::RowSchema,
    control: &ProductionControl<'_>,
) -> Result<u64, SQLError> {
    let mut bytes = 0_u64;
    for ty in schema.column_types() {
        control.check()?;
        bytes = bytes
            .checked_add(ty.as_ref().map_or(32, type_width))
            .ok_or_else(|| SQLError::Internal("EXPLAIN estimated row width overflow".into()))?;
    }
    Ok(bytes)
}

/// Use the SQL physical width where fixed, then declared bounds or the conventional variable-width estimate. These are plan estimates, never measured Rust allocations.
fn type_width(ty: &ColumnType) -> u64 {
    if let ColumnType::Domain { base, .. } = ty {
        return type_width(base);
    }
    if matches!(ty, ColumnType::Named(_)) {
        return 32;
    }
    let fixed = uqa_sql::catalog::type_metadata::pg_type_len(ty);
    if fixed > 0 {
        return fixed as u64;
    }
    let maximum = match ty {
        ColumnType::Character(characters) => return u64::from(*characters) * 4 + 4,
        ColumnType::Varchar(Some(characters)) => u64::from(*characters) * 4 + 4,
        ColumnType::Bit(Some(bits)) | ColumnType::VarBit(Some(bits)) => {
            u64::from(*bits).div_ceil(8) + 8
        }
        ColumnType::Numeric {
            precision: Some(precision),
            ..
        } => (u64::from(*precision) + 6) / 4 * 2 + 8,
        ColumnType::Vector(dimensions) => return u64::from(*dimensions) * 4,
        _ => return 32,
    };
    if maximum <= 32 {
        maximum
    } else {
        32 + (maximum.min(1000) - 32) / 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_widths_use_sql_widths_and_declared_bounds() {
        for (ty, expected) in [
            (ColumnType::SmallInteger, 2),
            (ColumnType::Integer, 4),
            (ColumnType::BigInteger, 8),
            (ColumnType::Real, 4),
            (ColumnType::DoublePrecision, 8),
            (ColumnType::TimeTz, 12),
            (ColumnType::Uuid, 16),
            (ColumnType::Text, 32),
            (ColumnType::Character(9), 40),
            (ColumnType::Varchar(Some(20)), 58),
            (ColumnType::Varchar(Some(10_000)), 516),
            (ColumnType::Bit(Some(17)), 11),
            (
                ColumnType::Numeric {
                    precision: Some(12),
                    scale: Some(2),
                },
                16,
            ),
            (ColumnType::Vector(1536), 6144),
        ] {
            assert_eq!(type_width(&ty), expected, "{ty:?}");
        }
    }
}
