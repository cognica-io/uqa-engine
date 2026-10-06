//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Anonymous-record descriptors retain analyzed field identities through query columns.

use super::{QueryPlan, SchemaScope};
use crate::{
    routines::RoutineResolution, schema::RecordFields, ColumnType, RowSchema, SQLError, SQLParam,
    ScalarExpr,
};

pub(super) fn input_fields(expression: &ScalarExpr, schema: &RowSchema) -> Option<RecordFields> {
    let slot = match expression {
        ScalarExpr::Column(name) => schema.column_slot(name),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            schema.qualified_slot(qualifier, column)
        }
        ScalarExpr::InternalColumn(column) => schema.internal_slot(*column),
        ScalarExpr::Position(position) => schema.slot(*position),
        _ => None,
    }?;
    schema.physical_record_fields(slot).cloned()
}

pub(super) fn common_fields<'a>(
    fields: impl IntoIterator<Item = Option<&'a RecordFields>>,
) -> Option<RecordFields> {
    let mut fields = fields.into_iter();
    let first = fields.next()??;
    fields
        .all(|other| other == Some(first))
        .then(|| first.clone())
}

pub(super) fn star_fields(
    expression: &ScalarExpr,
    schema: &RowSchema,
) -> Option<Vec<Option<RecordFields>>> {
    match expression {
        ScalarExpr::Star => Some(
            (0..schema.len())
                .filter(|index| schema.wildcard_position_visible(*index))
                .map(|index| schema.record_fields(index).cloned())
                .collect(),
        ),
        ScalarExpr::QualifiedStar(qualifier) => Some(
            schema
                .qualified_star_layout(qualifier)
                .iter()
                .map(|(_, slot, _)| schema.physical_record_fields(*slot).cloned())
                .collect(),
        ),
        _ => None,
    }
}

impl SchemaScope {
    /// Read the descriptor only after ordinary expression validation has succeeded. No value is evaluated, and column references reuse the input descriptor rather than infer fields from runtime values.
    pub(super) fn bind_record_fields(
        &mut self,
        routines: &dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<Option<RecordFields>, SQLError> {
        if let Some(fields) = input_fields(expression, schema) {
            return Ok(Some(fields));
        }
        match expression {
            ScalarExpr::Row(fields) => fields
                .iter()
                .map(|field| {
                    if matches!(
                        field,
                        ScalarExpr::Literal(uqa_core::Value::Str(_) | uqa_core::Value::Null)
                    ) {
                        Ok(None)
                    } else {
                        self.bind_expression_type(
                            routines,
                            field,
                            schema,
                            subqueries,
                            params,
                            Some(schema),
                        )
                    }
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|fields| Some(fields.into())),
            ScalarExpr::Cast { expr, ty, .. }
                if crate::type_resolution::canonical_routine_type_name(ty) == "record" =>
            {
                self.bind_record_fields(routines, expr, schema, subqueries, params)
            }
            ScalarExpr::Case {
                when, else_branch, ..
            } => {
                let mut fields = Vec::new();
                for expression in when
                    .iter()
                    .map(|(_, value)| value)
                    .chain(else_branch.iter().map(AsRef::as_ref))
                {
                    if !matches!(expression, ScalarExpr::Literal(uqa_core::Value::Null)) {
                        fields.push(self.bind_record_fields(
                            routines, expression, schema, subqueries, params,
                        )?);
                    }
                }
                Ok(common_fields(fields.iter().map(Option::as_ref)))
            }
            ScalarExpr::ScalarSubquery(index) => {
                let query = subqueries.get(*index).ok_or_else(|| {
                    SQLError::Internal("record subquery slot is outside its plan".into())
                })?;
                let output = self.bind_query(routines, query, params, Some(schema))?;
                Ok(output.record_fields(0).cloned())
            }
            _ => {
                let ty = self.bind_expression_type(
                    routines,
                    expression,
                    schema,
                    subqueries,
                    params,
                    Some(schema),
                )?;
                let Some(ColumnType::Composite(reference)) = ty else {
                    return Ok(None);
                };
                let descriptor =
                    crate::expr::composites::descriptor(routines.composite_types(), reference.oid)?;
                Ok(Some(
                    descriptor
                        .attributes
                        .iter()
                        .map(|attribute| Some(attribute.ty.clone()))
                        .collect::<Vec<_>>()
                        .into(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Routines;
    impl crate::FunctionTypeResolver for Routines {
        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&crate::ast::FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok(None)
        }
    }
    impl RoutineResolution for Routines {}

    #[test]
    fn record_fields_follow_ctes_derived_columns_and_identical_case_branches() {
        for sql in [
            "SELECT ROW(1,2::bigint)",
            "WITH r AS(SELECT ROW(1,2::bigint) AS v) SELECT v FROM r",
            "SELECT x.v FROM (SELECT ROW(1,2::bigint) AS v) x",
            "SELECT CASE WHEN true THEN ROW(1,2::bigint) ELSE ROW(3,4::bigint) END",
        ] {
            let crate::plan::UnifiedPlan::Query(query) =
                crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            let schema = crate::binding::analyze_query_plan_schema_with_catalog(
                &Routines,
                &query,
                &[],
                crate::binding::fixture::catalog(BTreeMap::new()),
                crate::binding::fixture::resolution(
                    vec!["public".into()],
                    "pg_temp_fixture".into(),
                ),
            )
            .unwrap();
            assert_eq!(
                schema.record_fields(0).unwrap().as_ref(),
                &[Some(ColumnType::Integer), Some(ColumnType::BigInteger)],
                "{sql}"
            );
        }
    }

    #[test]
    fn unknown_record_fields_remain_unknown_and_different_case_descriptors_remain_dynamic() {
        for (sql, expected) in [
            ("SELECT ROW('1',NULL)", Some(vec![None, None])),
            (
                "SELECT CASE WHEN true THEN ROW(1,2::bigint) ELSE ROW(3,4) END",
                None,
            ),
        ] {
            let crate::plan::UnifiedPlan::Query(query) =
                crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            let schema = crate::binding::analyze_query_plan_schema_with_catalog(
                &Routines,
                &query,
                &[],
                crate::binding::fixture::catalog(BTreeMap::new()),
                crate::binding::fixture::resolution(
                    vec!["public".into()],
                    "pg_temp_fixture".into(),
                ),
            )
            .unwrap();
            assert_eq!(
                schema.record_fields(0).map(|fields| fields.to_vec()),
                expected,
                "{sql}"
            );
        }
    }
}
