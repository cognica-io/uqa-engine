//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whole-row and system Vars retain the declaring relation's identities.

use super::{deparse, expressions, parse, Field, Routines};
use crate::{
    ast::{ColumnDef, CompositeTypeReference},
    schema::{ColumnTypeSchema, ScalarTypeSchema},
    ColumnType, SQLError, Statement,
};

struct Names<'a> {
    relation: &'a str,
    columns: &'a [ColumnDef],
}

impl deparse::ExpressionNames for Names<'_> {
    fn column(&self, attribute: i64) -> Result<String, SQLError> {
        self.columns
            .iter()
            .find(|column| column.attribute_number.map(i64::from) == Some(attribute))
            .map(|column| column.name.clone())
            .ok_or_else(|| SQLError::UnknownColumn(attribute.to_string()))
    }

    fn relation_name(&self) -> Result<String, SQLError> {
        Ok(self.relation.into())
    }

    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        deparse::ExpressionNames::routine(&Routines, oid)
    }

    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        if modifier == -1 {
            if let Some(ty) = u32::try_from(oid)
                .ok()
                .and_then(crate::catalog::type_metadata::builtin_scalar_type)
            {
                return Ok(ty.sql_name());
            }
        }
        deparse::ExpressionNames::type_name(&Routines, oid, modifier)
    }
}

#[test]
fn relation_scope_nodes_and_sql_match_postgresql() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("relation_scope.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["relation"].as_str().unwrap();
        let row_type = ColumnType::Composite(CompositeTypeReference {
            schema: "row_nodes".into(),
            name: name.into(),
            oid: u32::try_from(case["type_oid"].as_u64().unwrap()).unwrap(),
            array_oid: u32::try_from(case["array_oid"].as_u64().unwrap()).unwrap(),
            relation_oid: u32::try_from(case["relation_oid"].as_u64().unwrap()).unwrap(),
        });
        let columns = case["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|column| {
                let mut definition = ColumnDef::nullable(
                    column["name"].as_str().unwrap(),
                    ColumnType::from_sql_name(column["type"].as_str().unwrap()).unwrap(),
                );
                definition.attribute_number =
                    Some(i16::try_from(column["attribute"].as_i64().unwrap()).unwrap());
                definition
            })
            .collect::<Vec<_>>();
        let qualified = format!("row_nodes.{name}");
        let schema = ColumnTypeSchema::with_relation(&columns, &qualified, &row_type).unwrap();
        let physical = crate::RowSchema::with_column_definitions(Some(name), &columns)
            .with_relation_row_type(name, row_type.clone())
            .unwrap();
        assert!(std::ptr::eq(
            schema.whole_row_type(name).unwrap(),
            &raw const row_type
        ));
        assert_eq!(schema.type_of("tableoid"), Some(&ColumnType::Oid));
        assert_eq!(schema.collation_of("tableoid"), Some(0));
        let sql = format!("SELECT {}", case["expression"].as_str().unwrap());
        let Statement::Select(query) = crate::compile(&sql).unwrap().remove(0) else {
            panic!("SELECT expression");
        };
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: None,
            types: None,
            routines: &Routines,
        };
        let node = context.check(&query.projections[0].expr).unwrap();
        let expected = parse(case["node"].as_str().unwrap()).unwrap();
        assert_eq!(Field::Node(node.clone()), expected, "{name}: {sql}");
        let names = Names {
            relation: name,
            columns: &columns,
        };
        for pretty in [false, true] {
            assert_eq!(
                deparse::expression(&node.clone().into(), &names, pretty).unwrap(),
                case[if pretty { "pretty" } else { "sql" }]
                    .as_str()
                    .unwrap(),
                "{name}: {sql}, pretty={pretty}"
            );
        }
        if name != "shadow" {
            for scalar in [
                crate::ScalarExpr::Column(name.into()),
                crate::ScalarExpr::QualifiedStar(name.into()),
            ] {
                assert_eq!(
                    crate::scalar_type(&scalar, &schema, &[]).unwrap(),
                    Some(row_type.clone())
                );
                assert_eq!(
                    crate::scalar_type(&scalar, &physical, &[]).unwrap(),
                    Some(row_type.clone())
                );
            }
        }
    }
}
