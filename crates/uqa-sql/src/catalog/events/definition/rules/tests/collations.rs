//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    variables: Vec<Variable>,
}
#[derive(serde::Deserialize)]
struct Variable {
    side: String,
    attribute: usize,
    #[serde(rename = "type")]
    type_oid: u32,
    collation: u32,
}

#[test]
fn rule_condition_row_metadata_matches_postgresql_event_variables() {
    let catalog = collated_catalog();
    let references: Vec<Reference> = serde_json::from_str(include_str!("collations.json")).unwrap();
    assert_eq!(
        references
            .iter()
            .map(|case| case.variables.len())
            .sum::<usize>(),
        16
    );
    let scopes = Scope;
    let mut context = catalog.context();
    context.stored_routines.scopes = &scopes;
    let created = RelationLookupMode::Dynamic;
    for case in references {
        let Statement::CreateRule(rule) = crate::compile(&case.sql).unwrap().remove(0) else {
            panic!("rule")
        };
        let columns = catalog
            .context()
            .rule_relation_definitions("public.items")
            .unwrap();
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        let mut condition = rule.condition.unwrap();
        let (plan, binding) = context
            .validate_rule_condition(&mut condition, &columns, rule.event, None, None, created)
            .unwrap()
            .unwrap();
        assert_eq!(
            binding.referenced_columns(&plan).len(),
            case.variables.len()
        );
        let encoded = serde_json::to_vec(&(plan, binding)).unwrap();
        let (plan, binding): (crate::plan::ExpressionPlan, RuleConditionBinding) =
            serde_json::from_slice(&encoded).unwrap();
        let (plan, binding) = context
            .validate_rule_condition(
                &mut condition,
                &columns,
                rule.event,
                Some(&plan),
                Some(&binding),
                RelationLookupMode::Bound,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            binding.referenced_columns(&plan).len(),
            case.variables.len()
        );
        let schema = binding.row_schema(&columns);
        for variable in case.variables {
            let name = &names[variable.attribute - 1];
            let column = if variable.side == "old" {
                binding.old_column(name)
            } else {
                binding.new_column(name)
            }
            .unwrap();
            assert_eq!(
                schema.qualified_collation(&variable.side, name),
                Some(variable.collation),
                "{}: {}.{name}",
                case.sql,
                variable.side
            );
            let Expr::TypedLiteral {
                collation_oid, ty, ..
            } = bind_expr(
                &Expr::qualified_column(&variable.side, name),
                &mut RuleRowTypeResolver::for_condition(&columns, rule.event),
            )
            .unwrap()
            else {
                panic!("typed rule input")
            };
            let actual = collation_oid.unwrap_or_else(|| {
                u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
                    &crate::ColumnType::from_sql_name(&ty).unwrap(),
                ))
                .unwrap()
            });
            assert_eq!(actual, variable.collation);
            assert_eq!(
                schema.internal_collation(column),
                Some(variable.collation),
                "{}: {}.{name}",
                case.sql,
                variable.side
            );
            assert_eq!(
                crate::catalog::type_metadata::pg_type_oid(schema.internal_type(column).unwrap()),
                i64::from(variable.type_oid)
            );
        }
    }
}

struct Scope;
impl crate::binding::statements::StatementBindingScope for Scope {
    fn binding_context(&self) -> Result<crate::binding::BindingContext<'_>, SQLError> {
        Ok(crate::binding::fixture::empty_binding_context())
    }
}
impl crate::binding::stored_routines::analysis::CatalogRoutineScopes for Scope {
    fn with_catalog_scope(
        &self,
        analyze: crate::binding::statements::StatementAnalysisOperation<'_>,
    ) -> Result<(), SQLError> {
        analyze(self)
    }
}

#[test]
fn projected_rule_input_collations_remain_implicit() {
    let catalog = Catalog::default();
    for oid in [950, 951, 100, 0] {
        let metadata = crate::semantics::rules::binding::RuleColumnMetadata {
            ty: ColumnType::Text,
            collation_oid: Some(oid),
            uses_document_id: false,
            position: 0,
        };
        let crate::plan::UnifiedPlan::Query(mut source) = crate::plan::UnifiedPlan::lower(
            crate::compile("SELECT 'x' COLLATE \"C\"")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let crate::plan::RelationalPlan::QueryBlock(ref mut source) = source.root else {
            panic!("block")
        };
        let input = metadata.bind_input(source.projections.remove(0).expr);
        let crate::ScalarExpr::Collated { collation, .. } = &input else {
            panic!("column metadata")
        };
        assert_eq!(collation.oid(), oid);
        let crate::plan::UnifiedPlan::Query(mut query) = crate::plan::UnifiedPlan::lower(
            crate::compile("SELECT COALESCE($1, 'y' COLLATE \"POSIX\")")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        query.rewrite_scalar_expressions(&mut |expression| {
            if matches!(expression, crate::ScalarExpr::Param(1)) {
                *expression = input.clone();
            }
        });
        let schema = crate::binding::analyze_query_plan_schema(
            &catalog,
            &query,
            &[],
            &crate::binding::fixture::empty_binding_context(),
            None,
        )
        .unwrap();
        assert_eq!(schema.column_collation(0), Some(951));
    }
}
