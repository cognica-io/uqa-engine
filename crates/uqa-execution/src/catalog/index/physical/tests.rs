//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    ast::{Expr, TableConstraintSet, TableHierarchy},
    catalog::index::{EnforcedKey, IndexCatalogIdentity, IndexDefinition},
    RowSchema, SQLParam,
};

struct Context;

impl crate::schema::indexes::IndexBuildCatalog for Context {
    fn table_hierarchy(&self, _: &str) -> Result<TableHierarchy, SQLError> {
        panic!("same-table index needs no hierarchy lookup")
    }
    fn scan_tables(&self, _: &str) -> Result<Vec<String>, SQLError> {
        panic!("same-table index needs no hierarchy scan")
    }
}
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
            .indexable_fields(&Context, "public.t", &[], &[])
            .unwrap()
            .as_slice(),
        std::slice::from_ref(&original_key)
    );
    assert_eq!(
        values(&first, &original_key).unwrap(),
        Value::Row(vec![Value::Int(7)])
    );
    let replacement = cache.bind(rows(3, false)).unwrap();
    assert!(values(&replacement, &original_key).is_err());
    let new_key = ValueIndexKey::Index("opaque:3".into());
    assert_eq!(values(&replacement, &new_key).unwrap(), Value::Null);
    // Existing retained consumers and restored consumers see the same original definition.
    assert_eq!(
        values(&first, &original_key).unwrap(),
        Value::Row(vec![Value::Int(7)])
    );
    assert_eq!(
        values(&cache.bind(original).unwrap(), &original_key).unwrap(),
        Value::Row(vec![Value::Int(7)])
    );
}
