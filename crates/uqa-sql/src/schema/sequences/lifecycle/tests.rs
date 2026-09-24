//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;

#[test]
fn alter_binding_defers_kind_validation_until_after_authority() {
    for suffix in [
        "INCREMENT BY 2",
        "RENAME TO renamed",
        "SET SCHEMA target",
        "SET LOGGED",
    ] {
        let crate::Statement::AlterSequence(alter) =
            crate::compile(&format!("ALTER SEQUENCE s.wrong {suffix}"))
                .unwrap()
                .remove(0)
        else {
            panic!("expected sequence alteration");
        };
        assert_eq!(
            sequence_alter_relation(RelationResolution::Found("s.wrong".into(), "table"), &alter)
                .unwrap(),
            Some(("s.wrong".into(), "table"))
        );
        let error = validate_sequence_alter_kind(&alter, "table", "wrong").unwrap_err();
        assert_eq!(error.sqlstate(), Some("42809"));
        assert!(error
            .to_string()
            .contains(if suffix.starts_with("INCREMENT") {
                "cannot open relation \"wrong\""
            } else {
                "\"wrong\" is not a sequence"
            }));
        validate_sequence_alter_kind(&alter, "sequence", "wrong").unwrap();
    }
}

#[derive(Default)]
struct Catalog {
    owned: bool,
    collision: Cell<bool>,
    lookups: Cell<usize>,
}

impl SequenceLifecycleCatalog for Catalog {
    fn temporary_schema_name(&self) -> String {
        "pg_temp_42".into()
    }
    fn sequence_is_owned(&self, _: &RelationIdentity) -> bool {
        self.owned
    }
    fn relation_kind_at(&self, _: &str) -> Result<Option<&'static str>, String> {
        self.lookups.set(self.lookups.get() + 1);
        Ok(self.collision.get().then_some("table"))
    }
}

#[test]
fn destination_binding_defers_collisions_until_after_namespace_locking() {
    let catalog = Catalog::default();
    let source = RelationIdentity::new("public", "ids");
    let lifecycle = SequenceLifecycle::SetSchema {
        schema: "archive".into(),
    };
    catalog.collision.set(true);
    let target = sequence_lifecycle_target(&catalog, &source, &lifecycle).unwrap();
    assert_eq!(target, RelationIdentity::new("archive", "ids"));
    assert_eq!(catalog.lookups.get(), 0);
    catalog.collision.set(false);
    assert!(validate_sequence_lifecycle_target(
        &catalog,
        &source,
        &target,
        RelationPersistence::Permanent,
        &lifecycle,
    )
    .unwrap());
    catalog.collision.set(true);
    let error = validate_sequence_lifecycle_target(
        &catalog,
        &source,
        &target,
        RelationPersistence::Permanent,
        &lifecycle,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P07"));
    assert!(error
        .to_string()
        .contains("already exists in schema \"archive\""));
}

#[test]
fn owned_sequence_rejection_precedes_destination_lookup() {
    let catalog = Catalog {
        owned: true,
        ..Catalog::default()
    };
    for schema in ["missing", "pg_temp", "public"] {
        let error = sequence_lifecycle_target(
            &catalog,
            &RelationIdentity::new("public", "ids"),
            &SequenceLifecycle::SetSchema {
                schema: schema.into(),
            },
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert!(error.to_string().contains("cannot move an owned sequence"));
    }
    assert_eq!(catalog.lookups.get(), 0);
}

#[test]
fn unchanged_schema_still_validates_temporary_namespace_restrictions() {
    let catalog = Catalog::default();
    for (schema, persistence, expected) in [
        ("public", RelationPersistence::Permanent, None),
        (
            "pg_temp_42",
            RelationPersistence::Temporary,
            Some("temporary"),
        ),
        ("pg_toast", RelationPersistence::Permanent, Some("TOAST")),
    ] {
        let source = RelationIdentity::new(schema, "ids");
        let lifecycle = SequenceLifecycle::SetSchema {
            schema: schema.into(),
        };
        let result =
            validate_sequence_lifecycle_target(&catalog, &source, &source, persistence, &lifecycle);
        if let Some(fragment) = expected {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"));
            assert!(error.to_string().contains(fragment));
        } else {
            assert!(!result.unwrap());
        }
    }
    assert_eq!(catalog.lookups.get(), 0);
}

#[test]
fn rename_rejects_unchanged_names_but_allows_owned_temporary_sequences() {
    let catalog = Catalog {
        owned: true,
        ..Catalog::default()
    };
    let source = RelationIdentity::new("pg_temp_42", "ids");
    for name in ["ids", "\"renamed.ids\""] {
        let lifecycle = SequenceLifecycle::RenameTo { name: name.into() };
        let target = sequence_lifecycle_target(&catalog, &source, &lifecycle).unwrap();
        let result = validate_sequence_lifecycle_target(
            &catalog,
            &source,
            &target,
            RelationPersistence::Temporary,
            &lifecycle,
        );
        if name == "ids" {
            assert_eq!(result.unwrap_err().sqlstate(), Some("42P07"));
        } else {
            assert!(result.unwrap());
            assert_eq!(target, RelationIdentity::new("pg_temp_42", "renamed.ids"));
        }
    }
}

#[test]
fn destination_binding_preserves_explicit_public_and_quoted_temp_aliases() {
    let catalog = Catalog::default();
    for (schema, expected) in [
        ("public", "public"),
        ("\"pg_temp\"", "pg_temp"),
        ("\"a.b\"", "a.b"),
    ] {
        let target = sequence_lifecycle_target(
            &catalog,
            &RelationIdentity::new("source", "ids"),
            &SequenceLifecycle::SetSchema {
                schema: schema.into(),
            },
        )
        .unwrap();
        assert_eq!(target, RelationIdentity::new(expected, "ids"));
    }
}
