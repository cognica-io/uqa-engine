//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeSet;

#[derive(Default)]
struct Catalog {
    keys: Vec<TableKeyConstraint>,
    names: BTreeSet<String>,
    automatic: BTreeSet<String>,
    relations: BTreeSet<String>,
}

impl IndexNameCatalog for Catalog {
    fn automatic_constraint_names(&self, _: &str) -> Result<BTreeSet<String>, SQLError> {
        Ok(self.automatic.clone())
    }
    fn existing_constraint_keys(&self, _: &str) -> Result<Vec<TableKeyConstraint>, SQLError> {
        Ok(self.keys.clone())
    }
    fn existing_constraint_names(&self, _: &str) -> Result<BTreeSet<String>, SQLError> {
        Ok(self.names.clone())
    }
    fn relation_name_available(&self, name: &str) -> Result<bool, SQLError> {
        Ok(!self.relations.contains(name))
    }
}

fn key() -> TableKeyConstraint {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE t(v int UNIQUE)")
        .unwrap()
        .remove(0)
    else {
        panic!("table declaration")
    };
    table.key_constraints.into_iter().next().unwrap()
}

#[test]
fn default_index_names_preserve_quoted_components_and_expression_labels() {
    let catalog = Catalog::default();
    for (sql, expected) in [
        (
            "CREATE INDEX ON \"Mixed Table\"(\"Value!\")",
            "Mixed Table_Value!_idx",
        ),
        (
            "CREATE INDEX ON \"Mixed Table\"(lower(note))",
            "Mixed Table_lower_idx",
        ),
        (
            "CREATE INDEX ON \"Mixed Table\"(\"Value!\", \"Value!\")",
            "Mixed Table_Value!_Value!1_idx",
        ),
    ] {
        let crate::Statement::CreateIndex(index) = crate::compile(sql).unwrap().remove(0) else {
            panic!("index declaration")
        };
        assert_eq!(
            allocate_default_index_name(
                &catalog,
                &RelationIdentity::new("public", "Mixed Table"),
                &index.columns
            )
            .unwrap(),
            expected
        );
    }
}

#[test]
fn default_index_names_reclip_components_when_a_suffix_is_needed() {
    let table = RelationIdentity::new("public", "t".repeat(60));
    let keys = [crate::ast::IndexKey::Column("c".repeat(60))];
    let first = format!("{}_{}_idx", "t".repeat(29), "c".repeat(29));
    let catalog = Catalog {
        relations: [format!("public.{first}")].into(),
        ..Default::default()
    };
    assert_eq!(
        allocate_default_index_name(&Catalog::default(), &table, &keys).unwrap(),
        first
    );
    assert_eq!(
        allocate_default_index_name(&catalog, &table, &keys).unwrap(),
        format!("{}_{}_idx1", "t".repeat(29), "c".repeat(28))
    );
}

#[test]
fn automatic_key_names_skip_both_constraint_and_relation_names() {
    let catalog = Catalog {
        names: ["t_v_key", "t_v_key1"].map(str::to_owned).into(),
        relations: ["public.t_v_key2".into()].into(),
        ..Default::default()
    };
    let mut keys = [key()];
    name_constraint_indexes(&catalog, "public.t", &mut keys).unwrap();
    assert_eq!(keys[0].name.as_deref(), Some("t_v_key3"));
    for (name, state) in [("t_v_key", "42710"), ("t_v_key2", "42P07")] {
        keys[0].name = Some(name.into());
        assert_eq!(
            name_constraint_indexes(&catalog, "public.t", &mut keys)
                .unwrap_err()
                .sqlstate(),
            Some(state)
        );
    }
}

#[test]
fn an_existing_owner_keeps_its_current_name_after_structure_changes() {
    let mut candidate = key();
    candidate.name = Some("old".into());
    candidate.catalog_identity = Some(crate::ast::ConstraintCatalogIdentity {
        object_id: [1; 16],
        oid: 16384,
    });
    let mut current = candidate.clone();
    current.name = Some("renamed".into());
    let catalog = Catalog {
        keys: vec![current],
        names: ["renamed".into()].into(),
        ..Default::default()
    };
    candidate.columns = vec!["renamed_column".into()];
    let mut keys = [candidate];
    name_constraint_indexes(&catalog, "public.t", &mut keys).unwrap();
    assert_eq!(keys[0].name.as_deref(), Some("renamed"));
    assert_eq!(keys[0].columns, ["renamed_column"]);
}

#[test]
fn automatic_key_names_skip_schema_constraints_without_rejecting_explicit_names() {
    let catalog = Catalog {
        automatic: ["t_v_key", "t_v_key1"].map(str::to_owned).into(),
        relations: ["public.t_v_key2".into()].into(),
        ..Default::default()
    };
    let mut keys = [key()];
    name_constraint_indexes(&catalog, "public.t", &mut keys).unwrap();
    assert_eq!(keys[0].name.as_deref(), Some("t_v_key3"));
    keys[0].name = Some("t_v_key".into());
    name_constraint_indexes(&catalog, "public.t", &mut keys).unwrap();
    assert_eq!(keys[0].name.as_deref(), Some("t_v_key"));
}
