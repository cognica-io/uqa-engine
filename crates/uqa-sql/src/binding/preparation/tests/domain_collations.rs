//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain and array coercions retain the collation selected by their actual coercion nodes.

use super::*;

struct Domains;

fn domain(name: &str) -> ColumnType {
    let (oid, collation_oid) = match name {
        "d_posix" => (50_010, 951),
        "d_plain" => (50_020, 100),
        "d_array" => (50_030, 951),
        _ => panic!("unknown test domain"),
    };
    ColumnType::Domain {
        schema: "public".into(),
        name: name.into(),
        oid,
        array_oid: Some(oid + 1),
        collation_oid: Some(collation_oid),
        base: Box::new(if name == "d_array" {
            ColumnType::Array(Box::new(ColumnType::Text))
        } else {
            ColumnType::Text
        }),
    }
}

impl FunctionTypeResolver for Domains {
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
        let (name, array) = name
            .strip_suffix("[]")
            .map_or((name, false), |name| (name, true));
        let name = name.strip_prefix("public.").unwrap_or(name);
        Ok(matches!(name, "d_posix" | "d_plain" | "d_array").then(|| {
            let ty = domain(name);
            if array {
                ColumnType::Array(Box::new(ty))
            } else {
                ty
            }
        }))
    }
}
impl RoutineResolution for Domains {}

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    #[serde(default)]
    collations: Vec<u32>,
    error: Option<serde_json::Value>,
}

#[test]
fn domain_cast_collations_match_postgresql_for_literals_columns_and_identity_casts() {
    let crate::Statement::CreateTable(mut table) = crate::compile(
        "CREATE TABLE t(c text,p text,plain text,n integer,a text[],d text[],e text)",
    )
    .unwrap()
    .remove(0) else {
        panic!("table")
    };
    table.columns[0].collation_oid = Some(950);
    table.columns[1].collation_oid = Some(951);
    table.columns[4].collation_oid = Some(950);
    table.columns[5].ty = ColumnType::Array(Box::new(domain("d_posix")));
    table.columns[5].collation_oid = Some(950);
    table.columns[6].ty = domain("d_posix");
    table.columns[6].collation_oid = Some(950);
    let binding = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "t"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        ..crate::binding::fixture::empty_binding_context()
    };
    let cases: Vec<Reference> =
        serde_json::from_str(include_str!("domain_cast_collations.json")).unwrap();
    assert_eq!(cases.len(), 30);
    for case in cases {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let mut preparation = Preparation {
            routines: &Domains,
            scope: &mut SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let prepared = preparation.query(&query, None);
        let ordinary =
            crate::binding::analyze_query_plan_schema(&Domains, &query, &[], &binding, None);
        let mut results = vec![prepared, ordinary];
        if case.error.is_none() {
            results.push(crate::binding::bind_query_plan_schema(
                &Domains,
                &query,
                &[],
                &binding,
                None,
            ));
        }
        for result in results {
            if let Some(expected) = &case.error {
                let error = result
                    .err()
                    .unwrap_or_else(|| panic!("expected PostgreSQL error: {}", case.sql));
                assert_eq!(
                    error.sqlstate(),
                    expected["sqlstate"].as_str(),
                    "{}",
                    case.sql
                );
                assert_eq!(
                    error.to_string(),
                    expected["message"].as_str().unwrap(),
                    "{}",
                    case.sql
                );
                assert_eq!(error.detail(), expected["detail"].as_str(), "{}", case.sql);
                assert_eq!(error.hint(), expected["hint"].as_str(), "{}", case.sql);
            } else {
                let output = result.unwrap_or_else(|error| panic!("{}: {error}", case.sql));
                assert_eq!(output.len(), case.collations.len(), "{}", case.sql);
                for (position, oid) in case.collations.iter().enumerate() {
                    assert_eq!(
                        output.column_collation(position),
                        Some(*oid),
                        "{}",
                        case.sql
                    );
                }
            }
        }
    }
}
