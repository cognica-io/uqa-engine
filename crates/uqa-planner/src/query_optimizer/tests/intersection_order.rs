//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{Payload, PostingEntry, PostingList, Predicate, Value};
use uqa_operators::{
    ExecutionContext, FacetOperator, FilterOperator, IntersectOperator, Operator, OperatorTree,
};
use uqa_storage::{DocumentStore, MemoryDocumentStore};

use super::QueryOptimizer;

fn filter(field: &str, source: Option<OperatorTree>) -> OperatorTree {
    OperatorTree::Filter {
        field: field.into(),
        predicate: Predicate::Equals(Value::Bool(true)),
        source: source.map(Box::new),
    }
}

fn context() -> ExecutionContext {
    let mut documents = MemoryDocumentStore::new();
    documents
        .put(
            0,
            BTreeMap::from([
                ("keep".into(), Value::Bool(true)),
                ("other".into(), Value::Bool(true)),
                ("left".into(), Value::Str("l".into())),
                ("right".into(), Value::Str("r".into())),
            ]),
        )
        .unwrap();
    ExecutionContext::new().with_document_store(Arc::new(documents))
}

fn bind(tree: &OperatorTree) -> Arc<dyn Operator> {
    match tree {
        OperatorTree::Filter {
            field,
            predicate,
            source,
        } => Arc::new(FilterOperator::new(
            field.clone(),
            predicate.clone(),
            source.as_deref().map(bind),
        )),
        OperatorTree::Facet { field, source } => Arc::new(FacetOperator::new(
            field.clone(),
            source.as_deref().map(bind),
        )),
        OperatorTree::Intersect(children) => {
            Arc::new(IntersectOperator::new(children.iter().map(bind).collect()))
        }
        _ => panic!("unexpected intersection fixture operator"),
    }
}

#[test]
fn intersection_costs_preserve_right_biased_payload_fields() {
    let context = context();
    let tree = OperatorTree::Intersect(vec![
        filter(
            "keep",
            Some(OperatorTree::Facet {
                field: "left".into(),
                source: None,
            }),
        ),
        OperatorTree::Facet {
            field: "right".into(),
            source: None,
        },
    ]);
    let expected = PostingList::from_sorted_unchecked(vec![PostingEntry::new(
        0,
        Payload {
            score: 2.0,
            positions: Vec::new(),
            fields: BTreeMap::from([
                ("_facet_field".into(), Value::Str("right".into())),
                ("_facet_value".into(), Value::Str("r".into())),
                ("_facet_count".into(), Value::Int(1)),
            ]),
        },
    )]);
    assert_eq!(bind(&tree).execute(&context).unwrap(), expected);
    let optimized = QueryOptimizer::new().optimize(tree);
    assert_eq!(bind(&optimized).execute(&context).unwrap(), expected);
}

#[test]
fn membership_intersections_still_select_the_cheaper_operand_first() {
    let context = context();
    let tree = OperatorTree::Intersect(vec![
        filter("keep", Some(filter("other", None))),
        filter("keep", None),
    ]);
    let expected = bind(&tree).execute(&context).unwrap();
    assert_eq!(expected.entries().len(), 1);
    assert_eq!(expected.entries()[0].payload, Payload::default());
    let optimized = QueryOptimizer::new().optimize(tree);
    let OperatorTree::Intersect(children) = &optimized else {
        panic!("distinct membership operands must remain an intersection");
    };
    assert!(matches!(
        &children[0],
        OperatorTree::Filter { source: None, .. }
    ));
    assert_eq!(bind(&optimized).execute(&context).unwrap(), expected);
}
