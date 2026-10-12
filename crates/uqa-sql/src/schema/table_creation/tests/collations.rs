//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::analysis::{AnalysisCatalog, TableDefinition};
use crate::catalog::resolution::RelationNameResolution;
use std::sync::Arc;

struct Catalog;
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
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok((name == "d_posix").then(|| ColumnType::Domain {
            schema: "public".into(),
            name: name.into(),
            oid: 40001,
            array_oid: None,
            collation_oid: Some(951),
            base: Box::new(ColumnType::Text),
        }))
    }
}
impl crate::routines::RoutineResolution for Catalog {}
impl AnalysisCatalog for Catalog {
    fn table_name_resolved(
        &self,
        _: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        Ok((name == "t").then(|| "public.t".into()))
    }
    fn view_resolved(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<crate::catalog::analysis::ViewDefinition>, SQLError> {
        Ok(None)
    }
    fn foreign_table_resolved(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        Ok(None)
    }
    fn sequence_exists(&self, _: &RelationNameResolution, _: &str) -> Result<bool, SQLError> {
        Ok(false)
    }
    fn virtual_relation_schema(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
        Ok(None)
    }
    fn sql_functions(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<Arc<crate::routines::SQLUserFunction>>>, SQLError> {
        Ok(None)
    }
    fn table_resolved(
        &self,
        _: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        if name != "t" {
            return Ok(None);
        }
        let mut c = ColumnDef::nullable("c", ColumnType::Text);
        c.collation_oid = Some(950);
        let mut p = ColumnDef::nullable("p", ColumnType::Text);
        p.collation_oid = Some(951);
        Ok(Some(TableDefinition {
            columns: Arc::new(vec![c, p, ColumnDef::nullable("n", ColumnType::Integer)]),
            columns_declared: true,
        }))
    }
}

#[test]
fn query_defined_column_collations_and_errors_match_postgresql() {
    #[derive(serde::Deserialize)]
    struct Case {
        query: String,
        names: Vec<String>,
        kind: String,
        with_no_data: bool,
        #[serde(default)]
        columns: Vec<serde_json::Value>,
        error: Option<serde_json::Value>,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!("../collations.json")).unwrap();
    assert_eq!(cases.len(), 68);
    for case in cases {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(&case.query).unwrap().remove(0))
        else {
            panic!("query")
        };
        let result = crate::binding::analyze_query_plan_schema_with_catalog(
            &Catalog,
            &query,
            &[],
            Arc::new(Catalog),
            RelationNameResolution {
                search_path: vec!["public".into()],
                temporary_schema: "pg_temp_test".into(),
                temporary_namespace_allocated: false,
                current_user: "uqa".into(),
                lookup_mode: crate::catalog::resolution::RelationLookupMode::Dynamic,
            },
        )
        .and_then(|schema| {
            let columns = create_table_as_columns(&schema, &case.names)?;
            validate_create_table_as_columns(&Catalog, &columns)?;
            Ok(columns)
        });
        let context = format!(
            "{} no_data={}: {}",
            case.kind, case.with_no_data, case.query
        );
        if let Some(expected) = case.error {
            let error = result
                .err()
                .unwrap_or_else(|| panic!("expected error: {context}"));
            assert_eq!(error.sqlstate(), expected["sqlstate"].as_str(), "{context}");
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{context}"
            );
            assert_eq!(error.detail(), expected["detail"].as_str(), "{context}");
            assert_eq!(error.hint(), expected["hint"].as_str(), "{context}");
        } else {
            let columns = result.unwrap_or_else(|error| panic!("{context}: {error}"));
            assert_eq!(columns.len(), case.columns.len(), "{context}");
            for (column, expected) in columns.iter().zip(case.columns) {
                assert_eq!(column.name, expected["name"].as_str().unwrap(), "{context}");
                assert_eq!(
                    column.collation_oid.map(u64::from),
                    expected["collation"].as_u64(),
                    "{context}"
                );
            }
        }
    }
}
