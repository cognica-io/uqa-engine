//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CompositeTypeReference, FunctionBinding};
use crate::expr::composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor};
use std::sync::Arc;

struct Types;

impl FunctionTypeResolver for Types {
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

    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
}

impl CompositeTypeCatalog for Types {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        assert_eq!(oid, 20_001);
        Ok(Some(Arc::new(CompositeTypeDescriptor {
            type_oid: oid,
            relation_oid: 20_003,
            attributes: vec![CompositeAttribute {
                name: "b".into(),
                ty: ColumnType::Text,
                number: 3,
            }],
        })))
    }
}

fn composite() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: "pair".into(),
        oid: 20_001,
        array_oid: 20_002,
        relation_oid: 20_003,
    })
}

fn expression(sql: &str) -> ScalarExpr {
    let crate::plan::UnifiedPlan::Query(query) =
        crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block")
    };
    block.projections[0].expr.clone()
}

#[test]
fn named_fields_keep_catalog_attribute_numbers_through_domains_and_array_subscripts() {
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "wrapped".into(),
        oid: 20_004,
        array_oid: Some(20_005),
        base: Box::new(composite()),
    };
    for (sql, ty) in [
        ("SELECT (s.v).b", composite()),
        ("SELECT (s.v).b", domain),
        (
            "SELECT (s.v[1]).b",
            ColumnType::Array(Box::new(composite())),
        ),
    ] {
        let schema = RowSchema::with_qualified_types("s", vec!["v".into()], vec![Some(ty)]);
        assert_eq!(
            expression_composite_dependencies(&expression(sql), &schema, &[], &Types).unwrap(),
            [ObjectAddress::column(20_003, 3)]
        );
    }
}

#[test]
fn anonymous_records_and_whole_relation_rows_do_not_name_composite_attributes() {
    let schema = RowSchema::with_qualified_types("s", vec!["v".into()], vec![Some(composite())]);
    for sql in ["SELECT (s).v", "SELECT (ROW(1,'x')).f2", "SELECT s.v"] {
        assert!(
            expression_composite_dependencies(&expression(sql), &schema, &[], &Types)
                .unwrap()
                .is_empty()
        );
    }
}
