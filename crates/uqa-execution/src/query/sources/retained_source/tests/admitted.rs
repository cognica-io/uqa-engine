//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_sql::expr::composites::{
    CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor,
};

struct Catalog {
    added: Cell<bool>,
    reads: Cell<usize>,
}

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        let mut attributes = vec![CompositeAttribute {
            name: "a".into(),
            ty: ColumnType::Integer,
            number: 1,
            collation_oid: None,
        }];
        if self.added.get() {
            attributes.push(CompositeAttribute {
                name: "b".into(),
                ty: ColumnType::Text,
                number: 2,
                collation_oid: Some(950),
            });
        }
        self.reads.set(self.reads.get() + 1);
        Ok(Some(Arc::new(CompositeTypeDescriptor {
            type_oid: oid,
            relation_oid: 20_003,
            dropped: Vec::new(),
            attributes,
        })))
    }
}

impl uqa_sql::expr::EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn resolve_type_name(&self, _: &str) -> Result<Option<ColumnType>, String> {
        Ok(Some(ColumnType::Composite(
            uqa_sql::ast::CompositeTypeReference {
                schema: "public".into(),
                name: "pair".into(),
                oid: 20_001,
                array_oid: 20_002,
                relation_oid: 20_003,
            },
        )))
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
    fn cast_domain(
        &self,
        _: &Value,
        _: Option<&str>,
        _: &ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        panic!("admitted rows cannot rerun domain input")
    }
}

#[test]
fn retained_admitted_fields_read_current_types_only_when_the_source_is_built() {
    let relation = InternalRelationId::allocate();
    let schema =
        RowSchema::with_internal_relation_types(relation, vec![Some(ColumnType::Record); 2]);
    let value = Value::Record(vec![("a".into(), Value::Int(4))].into());
    let mut buffer = SpillBuffer::new(1);
    buffer
        .push(Batch::from_physical_rows(
            schema.clone(),
            vec![PhysicalRow::from_values(vec![value.clone(), value.clone()])],
        ))
        .unwrap();
    let rows = buffer.into_shared(schema).unwrap();
    let types: Arc<[Option<String>]> = vec![Some("public.pair".into()), None].into();
    let scope = RetainedRowScope::enter_admitted(relation, rows.clone(), types.clone()).unwrap();
    let catalog = Catalog {
        added: Cell::new(false),
        reads: Cell::new(0),
    };
    assert_eq!(catalog.reads.get(), 0);
    for added in [false, true, false] {
        catalog.added.set(added);
        let before = catalog.reads.get();
        let normalized = read_admitted_rows(
            rows.clone(),
            &types,
            &catalog,
            1,
            &uqa_core::CancellationToken::new(),
        )
        .unwrap();
        let mut reader = normalized.reader().unwrap();
        let batch = reader.next().unwrap().unwrap();
        let Some(Value::Record(fields)) = batch.rows[0].value(0) else {
            panic!("composite")
        };
        assert_eq!(fields.len(), if added { 2 } else { 1 });
        if added {
            assert_eq!(fields.get(1).unwrap(), &("b".into(), Value::Null));
        }
        assert_eq!(batch.rows[0].value(1), Some(&value));
        assert_eq!(catalog.reads.get(), before + 1);
        assert!(reader.next().is_none());
        assert_eq!(
            rows.reader().unwrap().next().unwrap().unwrap().rows[0].value(0),
            Some(&value)
        );
    }
    let token = uqa_core::CancellationToken::new();
    token.cancel();
    let before = catalog.reads.get();
    assert!(read_admitted_rows(rows, &types, &catalog, 1, &token).is_err());
    assert_eq!(catalog.reads.get(), before);
    drop(scope);
}
