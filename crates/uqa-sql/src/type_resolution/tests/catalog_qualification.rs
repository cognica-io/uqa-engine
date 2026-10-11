//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog selection and generated coercions preserve different type identities.

use super::super::*;

struct Catalog;

impl FunctionTypeResolver for Catalog {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(
            (name.trim_matches('"') == "text").then(|| ColumnType::Domain {
                schema: "first".into(),
                name: "text".into(),
                oid: 50_010,
                array_oid: Some(50_011),
                base: Box::new(ColumnType::Text),
            }),
        )
    }

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
}

fn expression(sql: &str) -> ScalarExpr {
    let crate::Statement::Select(mut select) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        unreachable!()
    };
    crate::plan::ExpressionPlan::lower(select.projections.remove(0).expr).scalar
}

#[test]
fn written_identifier_uses_catalog_selection_while_qualified_builtin_keeps_its_class() {
    for (sql, domain) in [("'ok'::text", true), ("'ok'::pg_catalog.text", false)] {
        let ty = scalar_type_with_resolver(&expression(sql), &RowSchema::default(), &[], &Catalog)
            .unwrap()
            .unwrap();
        assert_eq!(
            matches!(ty, ColumnType::Domain { oid: 50_010, .. }),
            domain,
            "{sql}"
        );
        if !domain {
            assert_eq!(ty, ColumnType::Text);
        }
    }
}

#[test]
fn selected_common_type_does_not_rebind_to_a_shadowing_domain() {
    let schema = RowSchema::with_types(vec!["s".into()], vec![Some(ColumnType::Text)]);
    let mut stored = expression("COALESCE(s, NULL)");
    crate::type_resolution::stored_constants::store_operand_coercions(
        &mut stored,
        &schema,
        &[],
        &Catalog,
    )
    .unwrap();
    assert_eq!(
        scalar_type_with_resolver(&stored, &schema, &[], &Catalog).unwrap(),
        Some(ColumnType::Text)
    );
    let once = stored.clone();
    assert!(
        !crate::type_resolution::stored_constants::store_operand_coercions(
            &mut stored,
            &schema,
            &[],
            &Catalog
        )
        .unwrap()
    );
    assert_eq!(stored, once);
}

#[test]
fn stored_identity_coercions_use_the_selected_type_and_exact_modifier() {
    let domain = Catalog.resolve_type_name("text").unwrap().unwrap();
    for (source, target, elided) in [
        (ColumnType::SmallInteger, "pg_catalog.\"int2\"", true),
        (ColumnType::SmallInteger, "pg_catalog.\"int4\"", false),
        (
            ColumnType::Varchar(Some(4)),
            "pg_catalog.\"varchar\"(4)",
            true,
        ),
        (
            ColumnType::Varchar(Some(4)),
            "pg_catalog.\"varchar\"(2)",
            false,
        ),
        (domain.clone(), "text", true),
        (domain, "pg_catalog.\"text\"", false),
        (ColumnType::Text, "text", false),
    ] {
        let schema = RowSchema::with_types(vec!["v".into()], vec![Some(source)]);
        let mut stored = ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Column("v".into())),
            ty: target.into(),
        };
        let changed = store_operand_coercions(&mut stored, &schema, &[], &Catalog).unwrap();
        assert_eq!(changed, elided, "{target}");
        assert_eq!(matches!(stored, ScalarExpr::Column(_)), elided, "{target}");
        let once = stored.clone();
        assert!(!store_operand_coercions(&mut stored, &schema, &[], &Catalog).unwrap());
        assert_eq!(stored, once);
    }
}

#[test]
fn lowered_stored_syntax_retains_cast_sites_through_coercion_binding() {
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    for sql in [
        "v::pg_catalog.int4",
        "(v::pg_catalog.int4)::pg_catalog.int4",
        "coalesce(v, v)::pg_catalog.int4",
        "coalesce(v::pg_catalog.int4, v)::pg_catalog.int4",
    ] {
        let crate::Statement::Select(mut select) =
            crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
        else {
            unreachable!()
        };
        let mut syntax = select.projections.remove(0).expr;
        let lowered = crate::plan::ExpressionPlan::lower(syntax.clone());
        let mut bound = lowered.clone();
        super::super::store_syntax_operand_coercions(&mut bound.scalar, &schema, &[], &Catalog)
            .unwrap();
        let sites =
            crate::binding::syntax_sites::expression_syntax_sites(&lowered, &bound).unwrap();
        crate::catalog::stored_ast::bind_stored_expression_sites(&mut syntax, &sites).unwrap();
        let restored = crate::plan::ExpressionPlan::lower(syntax);
        assert_eq!(restored.scalar, bound.scalar, "{sql}");
        let once = bound.scalar.clone();
        assert!(!super::super::store_syntax_operand_coercions(
            &mut bound.scalar,
            &schema,
            &[],
            &Catalog,
        )
        .unwrap());
        assert_eq!(bound.scalar, once, "{sql}");
    }
}
