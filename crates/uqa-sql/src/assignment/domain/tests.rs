//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{CompositeTypeReference, CreateDomain, DomainCheck, Expr},
    catalog::domain::{DomainCatalog, StoredDomain},
    expr::{
        composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor},
        EngineHook,
    },
};
use std::{cell::Cell, cell::RefCell, sync::Arc};

mod controlled;

struct Context {
    descriptor: RefCell<Arc<CompositeTypeDescriptor>>,
    checks: Cell<usize>,
}

fn base() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: "pair".into(),
        oid: 20_001,
        array_oid: 20_002,
        relation_oid: 20_003,
    })
}

impl Context {
    fn new() -> Self {
        Self {
            checks: Cell::new(0),
            descriptor: RefCell::new(Arc::new(CompositeTypeDescriptor {
                type_oid: 20_001,
                relation_oid: 20_003,
                dropped: vec![],
                attributes: [("a", ColumnType::Integer), ("b", ColumnType::Text)]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (name, ty))| CompositeAttribute {
                        name: name.into(),
                        ty,
                        collation_oid: None,
                        number: index as i16 + 1,
                    })
                    .collect(),
            })),
        }
    }
}

impl CompositeTypeCatalog for Context {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == 20_001).then(|| Arc::clone(&self.descriptor.borrow())))
    }
}

impl EngineHook for Context {
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
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        Ok((name == base().catalog_name()).then(base))
    }
    fn resolve_type_oid(&self, oid: u32) -> Result<Option<ColumnType>, String> {
        Ok((oid == 20_001).then(base))
    }
}

impl DomainCatalog for Context {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        (oid == 20_004).then(|| StoredDomain {
            object_id: [4; 16],
            oid,
            array_oid: None,
            identity: uqa_core::RelationIdentity::new("public", "checked_pair"),
            owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
            definition: CreateDomain {
                name: "public.checked_pair".into(),
                base: base(),
                collation: None,
                collation_oid: None,
                default: None,
                not_null: None,
                checks: vec![DomainCheck {
                    name: Some("rename".into()),
                    catalog_identity: None,
                    expression: Expr::Literal(Value::Bool(true)),
                    validated: true,
                }],
            },
            array_name: None,
            usage_acl: None,
        })
    }
}

impl AssignmentContext for Context {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        row: &dyn crate::expr::RowLookup,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        self.checks.set(self.checks.get() + 1);
        assert!(matches!(row.column("value"), Some(Value::Datum(_))));
        Arc::make_mut(&mut self.descriptor.borrow_mut()).attributes[0].name = "renamed".into();
        assert_eq!(
            crate::result::format_postgres_text(row.column("value").unwrap(), &base(), Some(self))?,
            "(7,x)"
        );
        Ok(Value::Bool(true))
    }
}

#[test]
fn domain_checks_observe_and_return_the_tuple_admitted_before_catalog_changes() {
    for assignment in [false, true] {
        let context = Context::new();
        let ty = context.domain_by_oid(20_004).unwrap().column_type();
        let value = Value::Record(uqa_core::RecordValue::from_parts(
            vec![
                ("a".into(), Value::Int(7)),
                ("b".into(), Value::Str("x".into())),
            ],
            Some(20_001),
        ));
        let result = convert_domain_value(
            &context,
            &value,
            Some(&base().catalog_name()),
            &ty,
            assignment,
        )
        .unwrap()
        .unwrap();
        assert_eq!(context.checks.get(), 1);
        assert_eq!(
            crate::result::format_postgres_text(&result, &ty, Some(&context)).unwrap(),
            "(7,x)"
        );
        assert_eq!(context.checks.get(), 1);
    }
}
