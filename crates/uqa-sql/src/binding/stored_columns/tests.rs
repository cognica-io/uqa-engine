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
