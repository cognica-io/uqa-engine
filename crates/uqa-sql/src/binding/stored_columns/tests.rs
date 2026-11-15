//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored VALUES expressions retain the same column identities as complete queries.

use super::*;

struct Catalog(Vec<String>);

impl StoredColumnCatalog for Catalog {
    fn stored_relation_column_names(&self, name: &str) -> Result<Option<Vec<String>>, SQLError> {
        Ok((name == "public.inputs").then(|| self.0.clone()))
    }
}

impl crate::routines::merge_columns::StoredMergeColumnCatalog for Catalog {
    fn stored_merge_target_definitions(&self, _: &str) -> Option<Vec<crate::ast::ColumnDef>> {
        None
    }
}

impl Catalog {
    fn context(&self) -> StoredColumnBindingContext<'_> {
        StoredColumnBindingContext {
            sources: self,
            merge: self,
        }
    }
}

#[test]
fn stored_distinct_on_keeps_output_aliases_and_qualified_input_columns() {
    let catalog = Catalog(vec!["original".into(), "other".into()]);
    for (distinct, expected) in [
        ("other", vec!["original"]),
        ("inputs.other", vec!["original", "other"]),
    ] {
        let mut statement = crate::compile(&format!(
            "SELECT DISTINCT ON ({distinct}) original AS other FROM public.inputs ORDER BY {distinct}"
        )).unwrap().remove(0);
        bind_rule_action_column_dependencies(catalog.context(), &mut statement).unwrap();
        let references = stored_statement_references(catalog.context(), &statement).unwrap();
        assert!(
            references.unresolved.is_empty(),
            "{:?}",
            references.unresolved
        );
        assert_eq!(
            references.dependencies,
            expected
                .into_iter()
                .map(|column| RuleColumnDependency {
                    relation: RelationIdentity::new("public", "inputs"),
                    column: column.into(),
                })
                .collect()
        );
        let Statement::Select(select) = statement else {
            panic!("query")
        };
        if distinct == "other" {
            assert!(matches!(&select.distinct_on[0], Expr::Column(name) if name == "other"));
        }
    }
}

#[test]
fn stored_quoted_relation_names_do_not_shadow_correlated_aliases() {
    struct Names;
    impl StoredColumnCatalog for Names {
        fn stored_relation_column_names(&self, _: &str) -> Result<Option<Vec<String>>, SQLError> {
            Ok(Some(vec!["id".into()]))
        }
    }
    impl crate::routines::merge_columns::StoredMergeColumnCatalog for Names {
        fn stored_merge_target_definitions(&self, _: &str) -> Option<Vec<crate::ast::ColumnDef>> {
            None
        }
    }
    let context = StoredColumnBindingContext {
        sources: &Names,
        merge: &Names,
    };
    for (query, relation) in [
        ("SELECT 1 FROM public.inputs AS b WHERE EXISTS (SELECT 1 FROM public.\"a.b\" WHERE b.id>0)", "public.inputs"),
        ("SELECT \"a.b\".id FROM public.\"a.b\"", "public.\"a.b\""),
        ("SELECT \"My Table\".id FROM public.\"My Table\"", "public.\"My Table\""),
        ("SELECT \"a\"\"b\".id FROM public.\"a\"\"b\"", "public.\"a\"\"b\""),
    ] {
        let mut statement = crate::compile(query).unwrap().remove(0);
        bind_rule_action_column_dependencies(context, &mut statement).unwrap();
        let references = stored_statement_references(context, &statement).unwrap();
        assert!(references.unresolved.is_empty(), "{query}: {:?}", references.unresolved);
        assert_eq!(references.dependencies, BTreeSet::from([RuleColumnDependency {
            relation: RelationIdentity::from_legacy_name(relation).unwrap(),
            column: "id".into(),
        }]), "{query}");
    }
}

#[test]
fn stored_lateral_join_tracks_preceding_columns_across_rename() {
    let original = Catalog(vec!["original".into(), "unused".into()]);
    let renamed = Catalog(vec!["renamed".into(), "unused".into()]);
    let relation = RelationIdentity::from_legacy_name("public.inputs").unwrap();
    for source in [
        "LATERAL (SELECT i.original AS n) r",
        "LATERAL (VALUES (i.original)) r(n)",
        "generate_series(i.original, i.original) r(n)",
    ] {
        let sql =
            format!("SELECT r.n FROM public.inputs i, {source} JOIN (VALUES (1)) b(m) ON true");
        let mut statement = crate::compile(&sql).unwrap().remove(0);
        bind_stored_statement_source_columns(&mut statement, |name| {
            original.stored_relation_column_names(name)
        })
        .unwrap();
        let reference = stored_statement_references(original.context(), &statement).unwrap();
        assert!(
            reference.unresolved.is_empty(),
            "{sql}: {:?}",
            reference.unresolved
        );
        assert_eq!(
            reference.dependencies,
            BTreeSet::from([RuleColumnDependency {
                relation: relation.clone(),
                column: "original".into(),
            }]),
            "{sql}"
        );
        assert!(
            rewrite_stored_statement_column(
                renamed.context(),
                &mut statement,
                &relation,
                "original",
                "renamed",
            )
            .unwrap(),
            "{sql}"
        );
        let restored: Statement =
            serde_json::from_str(&serde_json::to_string(&statement).unwrap()).unwrap();
        let reference = stored_statement_references(renamed.context(), &restored).unwrap();
        assert!(
            reference.unresolved.is_empty(),
            "{sql}: {:?}",
            reference.unresolved
        );
        assert_eq!(
            reference.dependencies,
            BTreeSet::from([RuleColumnDependency {
                relation: relation.clone(),
                column: "renamed".into(),
            }]),
            "{sql}"
        );
    }
}

#[test]
fn stored_values_bind_parameters_and_preserve_column_identity_across_rename() {
    let original = Catalog(vec!["original".into(), "unused".into()]);
    let relation = RelationIdentity::from_legacy_name("public.inputs").unwrap();
    for sql in [
        "VALUES ((SELECT original FROM public.inputs), argument)",
        "VALUES ((SELECT i.selected FROM public.inputs AS i(selected, other)), argument)",
        "VALUES ((SELECT original FROM public.inputs), argument) ORDER BY 1",
    ] {
        let mut statement = crate::compile(sql).unwrap().remove(0);
        bind_stored_statement_source_columns(&mut statement, |name| {
            original.stored_relation_column_names(name)
        })
        .unwrap();
        let reference = stored_statement_references(original.context(), &statement).unwrap();
        assert_eq!(
            reference.unresolved,
            vec![(None, "argument".into())],
            "{sql}"
        );
        assert_eq!(
            reference.dependencies,
            BTreeSet::from([RuleColumnDependency {
                relation: relation.clone(),
                column: "original".into(),
            }]),
            "{sql}",
        );
        let renamed = Catalog(vec!["renamed".into(), "unused".into()]);
        assert!(rewrite_stored_statement_column(
            renamed.context(),
            &mut statement,
            &relation,
            "original",
            "renamed",
        )
        .unwrap());
        let encoded = serde_json::to_string(&statement).unwrap();
        let restored: Statement = serde_json::from_str(&encoded).unwrap();
        let reference = stored_statement_references(renamed.context(), &restored).unwrap();
        assert_eq!(
            reference.unresolved,
            vec![(None, "argument".into())],
            "{sql}"
        );
        assert_eq!(
            reference.dependencies,
            BTreeSet::from([RuleColumnDependency {
                relation: relation.clone(),
                column: "renamed".into(),
            }]),
            "{sql}",
        );
    }
}
