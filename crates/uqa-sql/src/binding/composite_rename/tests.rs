//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, ColumnType, CompositeTypeReference},
    expr::composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor},
    FunctionTypeResolver,
};
use std::{collections::BTreeMap, sync::Arc};

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
impl RoutineResolution for Types {}
impl CompositeTypeCatalog for Types {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(Some(Arc::new(CompositeTypeDescriptor {
            type_oid: oid,
            relation_oid: oid + 2,
            attributes: vec![CompositeAttribute {
                name: "b".into(),
                ty: ColumnType::Text,
                number: 3,
            }],
        })))
    }
}

fn context() -> BindingContext<'static> {
    let mut context = super::super::fixture::empty_binding_context();
    context.catalog = super::super::fixture::catalog(
        [("first", 20001), ("second", 21001)]
            .into_iter()
            .map(|(name, oid)| {
                let column = ColumnDef::nullable(
                    "v",
                    ColumnType::Composite(CompositeTypeReference {
                        schema: "public".into(),
                        name: name.into(),
                        oid,
                        array_oid: oid + 1,
                        relation_oid: oid + 2,
                    }),
                );
                (
                    uqa_core::RelationIdentity::new("public", name),
                    super::super::fixture::table_definition(vec![column]),
                )
            })
            .collect::<BTreeMap<_, _>>(),
    );
    context
}

#[test]
fn renaming_uses_scoped_type_and_attribute_identity_in_joins_and_ctes() {
    let rename = CompositeFieldRename {
        target: 20001,
        number: 3,
        to: "label",
        routines: &Types,
        binding: context(),
    };
    for sql in [
        "SELECT (l.v).b AS left_field, (r.v).b AS right_field FROM first l CROSS JOIN second r",
        "WITH q AS (SELECT v FROM first) SELECT (l.v).b AS left_field, (r.v).b AS right_field FROM (SELECT v FROM q) l CROSS JOIN second r",
    ] {
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        assert!(rename.query(&mut query).unwrap());
        let mut names = Vec::new();
        query.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                if let ScalarExpr::Func {
                    binding: Some(binding),
                    args,
                    ..
                } = node
                {
                    if binding.dispatch == Some(crate::ast::FunctionDispatch::FieldSelect) {
                        if let Some(ScalarExpr::Literal(Value::Str(name))) = args.get(1) {
                            names.push(name.clone());
                        }
                    }
                }
            });
        });
        assert_eq!(names, ["label", "b"]);
        assert!(!rename.query(&mut query).unwrap());
    }
}
