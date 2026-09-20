//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

struct Types;
impl DomainTypeCatalog for Types {
    fn resolve_domain_type_reference(&self, name: &str) -> Option<ColumnType> {
        (name == "public.d").then(domain)
    }
}

fn domain() -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: "d".into(),
        oid: 16384,
        base: Box::new(ColumnType::Integer),
    }
}

#[test]
fn column_index_types_depend_on_the_column_instead_of_directly_on_the_domain() {
    let keys = [IndexKey::Column("v".into())];
    for owner in [None, Some([1; 16])] {
        let definition = IndexDefinition {
            key_types: vec![domain()],
            relationships: crate::catalog::index::IndexRelationships {
                owning_constraint: owner,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!index_directly_references_domain(
            &Types,
            &definition,
            &keys,
            &BTreeSet::from([16384])
        )
        .unwrap());
    }
}

#[test]
fn expression_result_types_retain_direct_domain_dependencies_at_their_key_position() {
    let keys = [
        IndexKey::Column("v".into()),
        IndexKey::Expression(Box::new(Expr::Literal(Value::Int(1)))),
    ];
    let mut definition = IndexDefinition {
        key_types: vec![domain(), ColumnType::Integer],
        ..Default::default()
    };
    let targets = BTreeSet::from([16384]);
    assert!(!index_directly_references_domain(&Types, &definition, &keys, &targets).unwrap());
    definition.key_types.swap(0, 1);
    assert!(index_directly_references_domain(&Types, &definition, &keys, &targets).unwrap());
    assert!(!index_directly_references_domain(
        &Types,
        &definition,
        &keys,
        &BTreeSet::from([16385])
    )
    .unwrap());
}

#[test]
fn index_expression_and_predicate_syntax_retain_direct_domain_dependencies() {
    let expression = Expr::Cast {
        expr: Box::new(Expr::Literal(Value::Int(1))),
        ty: "public.d".into(),
    };
    let mut definition = IndexDefinition::default();
    let targets = BTreeSet::from([16384]);
    assert!(index_directly_references_domain(
        &Types,
        &definition,
        &[IndexKey::Expression(Box::new(expression.clone()))],
        &targets,
    )
    .unwrap());
    definition.predicate = Some(Box::new(expression));
    assert!(index_directly_references_domain(
        &Types,
        &definition,
        &[IndexKey::Column("v".into())],
        &targets
    )
    .unwrap());
}
