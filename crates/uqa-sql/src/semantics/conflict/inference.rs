//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FunctionBinding, IndexKey, TableKeyConstraint, TableKeyConstraintKind},
    ColumnType, FunctionTypeResolver,
};

struct Catalog(Vec<ColumnDef>);
impl FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl RoutineResolution for Catalog {}
impl AggregateClassifier for Catalog {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}
impl InferenceBindingScope for Catalog {
    fn binding_scope(&self) -> Result<BindingSnapshot, SQLError> {
        Ok(crate::binding::fixture::empty_binding_context().into())
    }
}
impl ConflictCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(Some(self.0.clone()))
    }
    fn enforced_keys(&self, _: &str) -> Result<Vec<EnforcedKey>, String> {
        unreachable!("keys are supplied")
    }
    fn try_declared_table_constraints(&self, _: &str) -> Result<TableConstraintSet, String> {
        Ok(TableConstraintSet::default())
    }
}

fn stored_expression(expression: &mut crate::ast::Expr, catalog: &Catalog) {
    let lowered = ExpressionPlan::lower(expression.clone());
    let mut bound = lowered.clone();
    crate::binding::bind_expression_plan_routines_for_storage(
        catalog,
        &mut bound,
        &[],
        &crate::binding::fixture::empty_binding_context(),
        &RowSchema::with_column_definitions(None, &catalog.0),
    )
    .unwrap();
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&lowered, &bound).unwrap();
    crate::catalog::stored_ast::bind_stored_expression_sites(expression, &sites).unwrap();
}

fn reference_key(sql: &str, catalog: &Catalog) -> EnforcedKey {
    let crate::Statement::CreateIndex(mut index) = crate::compile(sql).unwrap().remove(0) else {
        panic!("index")
    };
    for key in &mut index.columns {
        if let IndexKey::Expression(expression) = key {
            stored_expression(expression, catalog);
        }
    }
    if let Some(predicate) = &mut index.predicate {
        stored_expression(predicate, catalog);
    }
    let mut key: EnforcedKey = TableKeyConstraint {
        catalog_identity: None,
        index_identity: None,
        name: None,
        kind: TableKeyConstraintKind::Unique,
        columns: index
            .columns
            .iter()
            .filter_map(|key| key.column().map(str::to_owned))
            .collect(),
        included_columns: vec![],
        nulls_not_distinct: false,
        without_overlaps: false,
    }
    .into();
    key.keys = index.columns;
    key.predicate = index.predicate;
    key
}

#[test]
fn analyzed_inference_and_stored_index_syntax_match_postgresql() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("inference.json")).unwrap();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let crate::Statement::CreateTable(table) = crate::compile(case["table"].as_str().unwrap())
            .unwrap()
            .remove(0)
        else {
            panic!("table")
        };
        let mut catalog = Catalog(table.columns);
        let declarations = table
            .element_order
            .iter()
            .filter_map(|element| match element {
                crate::ast::DeclaredElement::Column(declaration) => Some(declaration),
                _ => None,
            });
        for (column, declaration) in catalog.0.iter_mut().zip(declarations) {
            column.ty =
                crate::type_resolution::resolve_declared_column_type(&Catalog(vec![]), &column.ty)
                    .unwrap();
            column.collation_oid = Some(
                crate::catalog::collations::declarations::select(
                    &Catalog(vec![]),
                    &column.ty,
                    declaration.collation.as_deref(),
                )
                .unwrap(),
            );
        }
        let key = reference_key(case["index"].as_str().unwrap(), &catalog);
        let query = case["query"].as_str().unwrap();
        let crate::plan::UnifiedPlan::Command(command) =
            crate::plan::UnifiedPlan::lower(crate::compile(query).unwrap().remove(0))
        else {
            panic!("INSERT")
        };
        let crate::plan::CommandPlan::Insert(insert) = *command else {
            panic!("INSERT")
        };
        let context = InferenceContext {
            catalog: &catalog,
            aggregates: &catalog,
            routines: &catalog,
            binding: &catalog,
        };
        let prepared = prepare_inference_predicate(context, &insert, &[]).unwrap();
        let outcome = conflict_key_indices(
            &catalog,
            &insert.table,
            &[key],
            prepared.on_conflict.as_ref().unwrap(),
        );
        if case["error"].is_null() {
            assert_eq!(outcome.unwrap(), vec![0], "{query}");
        } else {
            let error = outcome.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                case["error"]["sqlstate"].as_str(),
                "{query}: {error}"
            );
            assert_eq!(
                error.to_string(),
                case["error"]["message"].as_str().unwrap(),
                "{query}"
            );
        }
    }
}
