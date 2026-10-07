//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    ast::{Expr, TableConstraintSet},
    catalog::index::{EnforcedKey, IndexCatalogIdentity, IndexDefinition},
    RowSchema, SQLParam,
};

struct Context;

impl uqa_sql::semantics::conflict::ConflictCatalog for Context {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(Some(Vec::new()))
    }
    fn enforced_keys(&self, _: &str) -> Result<Vec<EnforcedKey>, String> {
        unreachable!()
    }
    fn try_declared_table_constraints(&self, _: &str) -> Result<TableConstraintSet, String> {
        unreachable!()
    }
}
impl uqa_sql::semantics::partition::PartitionExpressions for Context {
    fn expression_text(&self, expression: &Expr) -> Result<String, SQLError> {
        uqa_sql::catalog::expression_text::schema_expr_text(expression)
    }
    fn evaluate_bound(&self, _: &Expr, _: &[SQLParam]) -> Result<Value, SQLError> {
        unreachable!()
    }
    fn evaluate_row(
        &self,
        expression: &Expr,
        _: &Document,
        _: &RowSchema,
        _: &[SQLParam],
    ) -> Result<Value, SQLError> {
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            _ => unreachable!(),
        }
    }
}

fn rows(incarnation: u8, predicate: bool) -> Arc<IndexRows> {
    let row = CatalogIndexRow {
        relation: RelationIdentity::new("public", "same_name"),
        table_name: "public.t".into(),
        index_type: "btree".into(),
        columns_json: serde_json::to_string(&vec![IndexKey::Expression(Box::new(Expr::Literal(
            Value::Int(7),
        )))])
        .unwrap(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&IndexDefinition {
                catalog: Some(IndexCatalogIdentity {
                    identity: uqa_core::catalog_identity::CatalogObjectIdentity {
                        object_id: [incarnation; 16],
                        oid: 17000 + i64::from(incarnation),
                    },
                    table_object_id: [1; 16],
                    physical_key: format!("opaque:{incarnation}"),
                }),
                predicate: Some(Box::new(Expr::Literal(Value::Bool(predicate)))),
                ..IndexDefinition::default()
            })
            .unwrap(),
        ),
    };
    BTreeMap::from([(row.relation.clone(), row)]).into()
}

fn values(definitions: &PhysicalIndexDefinitions, key: &ValueIndexKey) -> Result<Value, SQLError> {
    let context = Context;
    definitions
        .document_values(
            IndexExpressionContext {
                catalog: &context,
                expressions: &context,
            },
            "public.t",
            std::slice::from_ref(key),
            &Document::new(),
        )
        .map(|mut values| values.remove(key).unwrap())
}

#[test]
fn opaque_bindings_follow_registry_replacement_and_rollback_without_name_lookup() {
    let cache = PhysicalIndexCache::default();
    let original = rows(2, true);
    let first = cache.bind(original.clone()).unwrap();
    assert!(Arc::ptr_eq(&first, &cache.bind(original.clone()).unwrap()));
    let original_key = ValueIndexKey::Index("opaque:2".into());
    assert_eq!(
        first
            .indexable_fields("public.t", &[], &[])
            .unwrap()
            .as_slice(),
        std::slice::from_ref(&original_key)
    );
    assert_eq!(
        values(&first, &original_key).unwrap(),
        Value::Row(vec![Value::Int(7)].into())
    );
    let replacement = cache.bind(rows(3, false)).unwrap();
    assert!(values(&replacement, &original_key).is_err());
    let new_key = ValueIndexKey::Index("opaque:3".into());
    assert_eq!(values(&replacement, &new_key).unwrap(), Value::Null);
    // Existing retained consumers and restored consumers see the same original definition.
    assert_eq!(
        values(&first, &original_key).unwrap(),
        Value::Row(vec![Value::Int(7)].into())
    );
    assert_eq!(
        values(&cache.bind(original).unwrap(), &original_key).unwrap(),
        Value::Row(vec![Value::Int(7)].into())
    );
}

#[test]
fn publication_barriers_follow_expression_predicate_and_catalog_replacement() {
    let source = rows(2, true);
    let cache = PhysicalIndexCache::default();
    let expressions = cache.bind(source.clone()).unwrap();
    assert!(expressions.row_publication_uses_expressions("public.t"));
    assert!(!expressions.row_publication_uses_expressions("public.other"));
    let mut columns = source.as_ref().clone();
    for row in columns.values_mut() {
        row.columns_json = serde_json::to_string(&[IndexKey::Column("id".into())]).unwrap();
    }
    let predicates = cache.bind(Arc::new(columns.clone())).unwrap();
    assert!(predicates.row_publication_uses_expressions("public.t"));
    for row in columns.values_mut() {
        let mut definition = super::super::index_definition(row).unwrap();
        definition.predicate = None;
        row.definition_json = Some(serde_json::to_string(&definition).unwrap());
    }
    let plain = cache.bind(Arc::new(columns)).unwrap();
    assert!(!plain.row_publication_uses_expressions("public.t"));
    assert!(cache
        .bind(source)
        .unwrap()
        .row_publication_uses_expressions("public.t"));
}

#[test]
fn legacy_partition_namespaces_are_scoped_to_their_physical_tables() {
    let mut source = rows(2, true).as_ref().clone();
    let mut second = source.values().next().unwrap().clone();
    second.relation.name = "child_index".into();
    second.table_name = "public.child".into();
    second.columns_json = serde_json::to_string(&vec![IndexKey::Expression(Box::new(
        Expr::Literal(Value::Int(9)),
    ))])
    .unwrap();
    let mut definition = super::super::index_definition(&second).unwrap();
    definition.catalog.as_mut().unwrap().identity.object_id = [3; 16];
    definition.catalog.as_mut().unwrap().identity.oid = 17003;
    definition.catalog.as_mut().unwrap().table_object_id = [4; 16];
    second.definition_json = Some(serde_json::to_string(&definition).unwrap());
    source.insert(second.relation.clone(), second);
    let prepared = PhysicalIndexDefinitions::prepare(&source).unwrap();
    let key = ValueIndexKey::Index("opaque:2".into());
    assert_eq!(
        values(&prepared, &key).unwrap(),
        Value::Row(vec![Value::Int(7)].into())
    );
    let child = prepared
        .document_values(
            IndexExpressionContext {
                catalog: &Context,
                expressions: &Context,
            },
            "public.child",
            std::slice::from_ref(&key),
            &Document::new(),
        )
        .unwrap();
    assert_eq!(child[&key], Value::Row(vec![Value::Int(9)].into()));
}

fn btree(name: &str, key: u8, keys: Vec<IndexKey>, included: &[&str]) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", name),
        table_name: "public.t".into(),
        index_type: "btree".into(),
        columns_json: serde_json::to_string(&keys).unwrap(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&IndexDefinition {
                catalog: Some(IndexCatalogIdentity {
                    identity: uqa_core::catalog_identity::CatalogObjectIdentity {
                        object_id: [key; 16],
                        oid: 18000 + i64::from(key),
                    },
                    table_object_id: [1; 16],
                    physical_key: format!("opaque:{key}"),
                }),
                included_columns: included.iter().map(|name| (*name).to_owned()).collect(),
                ..IndexDefinition::default()
            })
            .unwrap(),
        ),
    }
}

fn column(name: &str) -> ValueIndexKey {
    ValueIndexKey::Column(name.into())
}

#[test]
fn every_plain_key_column_is_searched_and_included_columns_are_carried() {
    let rows = [
        btree(
            "composite",
            1,
            vec![IndexKey::Column("a".into()), IndexKey::Column("b".into())],
            &["c", "d"],
        ),
        btree(
            "expression",
            2,
            vec![
                IndexKey::Expression(Box::new(Expr::Literal(Value::Int(7)))),
                IndexKey::Column("e".into()),
            ],
            &["a"],
        ),
        btree("leading", 3, vec![IndexKey::Column("d".into())], &[]),
    ]
    .into_iter()
    .map(|row| (row.relation.clone(), row))
    .collect::<IndexRows>();
    let definitions = PhysicalIndexDefinitions::prepare(&rows).unwrap();
    let search = definitions.search_fields("public.t", &[], &[]);
    assert_eq!(
        search.into_iter().collect::<Vec<_>>(),
        [
            column("a"),
            column("b"),
            column("d"),
            column("e"),
            ValueIndexKey::Index("opaque:2".into()),
        ]
    );
    // `a` and `d` are search keys of other indexes, so only `c` is carried.
    assert_eq!(
        definitions
            .carried_fields("public.t", &[], &[])
            .into_iter()
            .collect::<Vec<_>>(),
        [column("c")]
    );
    assert_eq!(
        definitions.indexable_fields("public.t", &[], &[]).unwrap(),
        [
            column("a"),
            column("b"),
            column("c"),
            column("d"),
            column("e"),
            ValueIndexKey::Index("opaque:2".into()),
        ]
    );
    assert!(definitions
        .indexable_fields("public.other", &[], &[])
        .unwrap()
        .is_empty());
}

#[test]
fn composite_unique_keys_retain_the_same_complete_tuple_in_storage_and_commands() {
    for predicate in [None, Some(true), Some(false)] {
        let mut row = btree(
            "unique_pair",
            4,
            vec![IndexKey::Column("a".into()), IndexKey::Column("b".into())],
            &["payload"],
        );
        let mut definition = super::super::index_definition(&row).unwrap();
        definition.unique = true;
        definition.predicate = predicate.map(|value| Box::new(Expr::Literal(Value::Bool(value))));
        row.definition_json = Some(serde_json::to_string(&definition).unwrap());
        let definitions =
            PhysicalIndexDefinitions::prepare(&BTreeMap::from([(row.relation.clone(), row)]))
                .unwrap();
        let physical = ValueIndexKey::Index("opaque:4".into());
        assert!(definitions
            .search_fields("public.t", &[], &[])
            .contains(&physical));
        let context = IndexExpressionContext {
            catalog: &Context,
            expressions: &Context,
        };
        let document = Document::from([
            ("a".into(), Value::Int(7)),
            ("payload".into(), Value::Str("not part of the key".into())),
        ]);
        let expected = if predicate == Some(false) {
            Value::Null
        } else {
            Value::Row(vec![Value::Int(7), Value::Null].into())
        };
        let stored = definitions
            .document_values(
                context,
                "public.t",
                std::slice::from_ref(&physical),
                &document,
            )
            .unwrap();
        let staged = definitions
            .command_expression_values(context, "public.t", &document)
            .unwrap();
        assert_eq!(stored[&physical], expected);
        assert_eq!(staged["opaque:4"], expected);
    }
}
