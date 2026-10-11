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
