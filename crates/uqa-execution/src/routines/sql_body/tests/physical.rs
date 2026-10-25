//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::row_fields;
use std::sync::Arc;
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{
    ast::CompositeTypeReference,
    expr::{
        composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor},
        EngineHook,
    },
    ColumnType, SQLError,
};

struct Catalog(Arc<CompositeTypeDescriptor>);

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == self.0.type_oid).then(|| self.0.clone()))
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
    fn resolve_type_oid(&self, oid: u32) -> Result<Option<ColumnType>, String> {
        Ok((oid == self.0.type_oid).then(|| {
            ColumnType::Composite(CompositeTypeReference {
                schema: "public".into(),
                name: "returned".into(),
                oid,
                relation_oid: self.0.relation_oid,
                array_oid: oid + 2,
            })
        }))
    }
}

#[test]
fn routine_row_shaping_reads_retained_composites_without_type_input() {
    let catalog = Catalog(Arc::new(CompositeTypeDescriptor {
        type_oid: 16_384,
        relation_oid: 16_385,
        dropped: vec![],
        attributes: vec![CompositeAttribute {
            name: "n".into(),
            number: 1,
            collation_oid: None,
            ty: ColumnType::Integer,
        }],
    }));
    let control = ProductionControl::uncontrolled();
    let value = uqa_sql::expr::composites::constructor::construct_current_with_control(
        &catalog.0,
        Some(&catalog),
        &control,
        |_| Ok(control.copy_value(&Value::Int(7))?),
    )
    .unwrap()
    .into_uncontrolled()
    .unwrap();
    let fields = row_fields(value.clone(), 1, Some(&catalog)).unwrap();
    let Value::Datum(field) = &fields[0] else {
        panic!("deferred field");
    };
    assert_eq!(uqa_sql::expr::datums::read(field).unwrap(), Value::Int(7));
    assert_eq!(
        row_fields(value, 2, Some(&catalog)).unwrap_err().sqlstate(),
        Some("42804")
    );
}
