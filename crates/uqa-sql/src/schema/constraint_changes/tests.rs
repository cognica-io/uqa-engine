//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER publication follows independent `PostgreSQL` physical-tuple observations.

use super::*;
use crate::ast::{AlterTableAction, TableConstraintSet};

#[derive(serde::Deserialize)]
struct Observation {
    sql: String,
    replaced: bool,
}

#[test]
fn constraint_alteration_reports_postgresql_tuple_replacements() {
    let observations: Vec<Observation> =
        serde_json::from_str(include_str!("alteration.json")).unwrap();
    for foreign_key in [
        "ref integer, CONSTRAINT f FOREIGN KEY(ref) REFERENCES p(id)",
        "ref integer CONSTRAINT f REFERENCES p(id)",
    ] {
        let declaration =
            format!("CREATE TABLE c(id integer CONSTRAINT n NOT NULL, {foreign_key})");
        let crate::Statement::CreateTable(table) = crate::compile(&declaration).unwrap().remove(0)
        else {
            panic!("expected table declaration");
        };
        let mut columns = table.columns;
        let mut constraints = TableConstraintSet {
            foreign_keys: table.foreign_keys,
            ..TableConstraintSet::default()
        };
        for observation in &observations {
            let crate::Statement::AlterTable(mut statement) =
                crate::compile(&observation.sql).unwrap().remove(0)
            else {
                panic!("expected table alteration");
            };
            let AlterTableAction::AlterConstraint {
                name,
                enforceability,
                deferrability,
                no_inherit,
            } = statement.actions.remove(0)
            else {
                panic!("expected constraint alteration");
            };
            let effects = apply_constraint_alteration(
                &statement.table,
                &name,
                &mut columns,
                &mut constraints,
                ConstraintAlterOptions {
                    enforceability,
                    deferrability,
                    no_inherit,
                    ancestor: None,
                },
            )
            .unwrap();
            assert_eq!(effects.changed, observation.replaced, "{}", observation.sql);
            assert_eq!(
                effects.validate_after_publish,
                observation.sql == "ALTER TABLE c ALTER CONSTRAINT f ENFORCED"
                    && observation.replaced,
                "{}",
                observation.sql,
            );
        }
    }
}
