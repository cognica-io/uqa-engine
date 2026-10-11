//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation dependencies follow the selected regclass identity and its OID.

use super::*;
use crate::ast::{ColumnType, Statement};

struct Relations;

impl RoutineRelationOids for Relations {
    fn bound_regclass_oid(&self, name: &str) -> Result<Option<i64>, SQLError> {
        Ok((name == "public.target").then_some(42))
    }
}

#[test]
fn qualified_catalog_regclass_retains_relation_dependencies() {
    let Statement::CreateFunction(mut definition) = crate::compile(
        "CREATE FUNCTION sequence_value() RETURNS bigint LANGUAGE SQL RETURN nextval('target'::pg_catalog.regclass)",
    )
    .unwrap()
    .remove(0) else {
        panic!("routine definition")
    };
    let FunctionBody::Statements(statements) = &mut definition.body else {
        panic!("stored SQL body")
    };
    for statement in statements {
        crate::catalog::stored_ast::visit_stored_statement_expressions(statement, &mut |node| {
            if matches!(node, Expr::Literal(Value::Str(_))) {
                *node = Expr::TypedLiteral {
                    composite_source: None,
                    value: Value::Int(42),
                    ty: ColumnType::Regclass.catalog_name(),
                };
            }
            Ok(())
        })
        .unwrap();
    }
    assert!(stored_routine_references_relations(
        &Relations,
        &definition,
        &BTreeSet::from(["public.target".into()]),
    )
    .unwrap());
    assert!(!stored_routine_references_relations(
        &Relations,
        &definition,
        &BTreeSet::from(["public.other".into()]),
    )
    .unwrap());
    assert!(!is_regclass(&ColumnType::Regtype.catalog_name()));
    assert!(!is_regclass("domain#50001"));
}
