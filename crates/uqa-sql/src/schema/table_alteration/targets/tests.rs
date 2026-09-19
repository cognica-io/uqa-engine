//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn statement(sql: &str) -> AlterTableStmt {
    let crate::Statement::AlterTable(statement) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected ALTER TABLE: {sql}");
    };
    statement
}

#[test]
fn target_resolution_preserves_actual_kind_before_action_validation() {
    let statement = statement("ALTER TABLE s.items ADD COLUMN extra integer");
    let target = table_alter_target(
        RelationResolution::Found("s.items".into(), "sequence"),
        &statement,
        &mut |_| panic!("an existing target emits no notice"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(target.kind, "sequence");
    assert_eq!(target.relation.qualified_name(), "s.items");
    let Err(error) = bind_table_alteration(target, statement) else {
        panic!("sequences do not accept table columns");
    };
    assert_eq!(error.sqlstate(), Some("42809"));
}

#[test]
fn missing_table_and_schema_keep_their_diagnostics_for_every_action() {
    for action in [
        "ADD COLUMN extra integer",
        "OWNER TO reader",
        "RENAME TO renamed",
    ] {
        for (name, resolution, state, message) in [
            (
                "missing",
                RelationResolution::MissingRelation,
                "42P01",
                "relation \"missing\" does not exist",
            ),
            (
                "absent.missing",
                RelationResolution::MissingSchema("absent".into()),
                "3F000",
                "schema \"absent\" does not exist",
            ),
        ] {
            let statement = statement(&format!("ALTER TABLE {name} {action}"));
            let Err(error) =
                table_alter_target(resolution, &statement, &mut |_| panic!("no IF EXISTS"))
            else {
                panic!("expected a missing source error");
            };
            assert_eq!(error.sqlstate(), Some(state));
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}

#[test]
fn if_exists_emits_one_notice_for_the_local_requested_name() {
    for resolution in [
        RelationResolution::MissingRelation,
        RelationResolution::MissingSchema("absent".into()),
    ] {
        let statement = statement("ALTER TABLE IF EXISTS absent.missing ADD COLUMN extra integer");
        let mut notices = Vec::new();
        assert!(
            table_alter_target(resolution, &statement, &mut |message| notices
                .push(message.to_string()))
            .unwrap()
            .is_none()
        );
        assert_eq!(notices, ["relation \"missing\" does not exist, skipping"]);
    }
}

#[test]
fn historical_table_owner_syntax_lowers_to_both_view_kinds() {
    for (kind, expected) in [
        ("view", crate::ast::AlterViewKind::View),
        (
            "materialized view",
            crate::ast::AlterViewKind::MaterializedView,
        ),
    ] {
        let statement = statement("ALTER TABLE items OWNER TO reader");
        let target = RelationAlterTarget::from_name("s.items".into(), kind).unwrap();
        let BoundTableAlteration::View(bound) = bind_table_alteration(target, statement).unwrap()
        else {
            panic!("expected a native view owner change");
        };
        assert_eq!(bound.name, "s.items");
        assert_eq!(bound.kind, expected);
        assert!(
            matches!(bound.action, crate::ast::AlterViewAction::OwnerTo(crate::ast::RoleSpecification::Named(name)) if name == "reader")
        );
    }
}
