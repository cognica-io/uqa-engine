//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER, CTAS, SELECT INTO, lifecycle, and analysis-order compilation.

use super::*;

#[test]
fn trigger_statements_preserve_postgresql_event_and_lifecycle_shape() {
    let Statement::CreateTrigger(trigger) = first(
        "CREATE OR REPLACE TRIGGER normalize_before BEFORE INSERT OR UPDATE OF title, body ON app.items FOR EACH ROW WHEN (NEW.title IS NOT NULL) EXECUTE FUNCTION app.normalize('first', 'second')",
    ) else {
        panic!("expected CREATE TRIGGER");
    };
    assert_eq!(trigger.name, "normalize_before");
    assert_eq!(trigger.table, "app.items");
    assert_eq!(trigger.function, "app.normalize");
    assert_eq!(trigger.arguments, ["first", "second"]);
    assert!(!trigger.constraint);
    assert_eq!(trigger.referenced_table, None);
    assert_eq!(
        trigger.deferrability,
        crate::ast::TriggerDeferrability::NotDeferrable
    );
    assert!(trigger.row);
    assert!(trigger.or_replace);
    assert_eq!(trigger.timing, crate::ast::TriggerTiming::Before);
    assert_eq!(
        trigger.events,
        [
            crate::ast::TriggerEvent::Insert,
            crate::ast::TriggerEvent::Update,
        ]
    );
    assert_eq!(trigger.update_columns, ["title", "body"]);
    assert!(trigger.when.is_some());

    let Statement::DropTrigger(drop) =
        first("DROP TRIGGER IF EXISTS normalize_before ON app.items CASCADE")
    else {
        panic!("expected DROP TRIGGER");
    };
    assert_eq!(drop.name, "normalize_before");
    assert_eq!(drop.table, "app.items");
    assert!(drop.if_exists);
    assert!(drop.cascade);

    let Statement::AlterTable(rename) =
        first("ALTER TRIGGER normalize_before ON app.items RENAME TO normalized_before")
    else {
        panic!("expected ALTER TRIGGER");
    };
    assert!(matches!(
        rename.actions.as_slice(),
        [AlterTableAction::RenameTrigger { from, to }]
            if from == "normalize_before" && to == "normalized_before"
    ));

    let Statement::AlterTable(rename_constraint) =
        first("ALTER TABLE app.items RENAME CONSTRAINT guarded TO guarded_v2")
    else {
        panic!("expected ALTER TABLE RENAME CONSTRAINT");
    };
    assert!(matches!(
        rename_constraint.actions.as_slice(),
        [AlterTableAction::RenameConstraint { from, to }]
            if from == "guarded" && to == "guarded_v2"
    ));

    let Statement::AlterTable(enable) =
        first("ALTER TABLE app.items ENABLE ALWAYS TRIGGER normalized_before")
    else {
        panic!("expected ALTER TABLE ENABLE TRIGGER");
    };
    assert!(matches!(
        enable.actions.as_slice(),
        [AlterTableAction::SetTriggerEnableMode {
            name: Some(name),
            mode: crate::ast::EventEnableMode::Always,
            ..
        }] if name == "normalized_before"
    ));
}

#[test]
fn constraint_trigger_shape_is_preserved_for_execution_validation() {
    let Statement::CreateTrigger(trigger) = first(
        "CREATE CONSTRAINT TRIGGER constrained AFTER INSERT OR UPDATE ON app.items FROM app.parents DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION app.probe()",
    ) else {
        panic!("expected CREATE CONSTRAINT TRIGGER");
    };
    assert!(trigger.constraint);
    assert_eq!(trigger.referenced_table.as_deref(), Some("app.parents"));
    assert_eq!(
        trigger.deferrability,
        crate::ast::TriggerDeferrability::InitiallyDeferred
    );
    assert!(trigger.row);
    assert_eq!(trigger.timing, crate::ast::TriggerTiming::After);
    assert_eq!(
        trigger.events,
        [
            crate::ast::TriggerEvent::Insert,
            crate::ast::TriggerEvent::Update,
        ]
    );
}

#[test]
fn trigger_transition_relations_preserve_names_and_level() {
    let Statement::CreateTrigger(statement) = first(
        "CREATE TRIGGER transitioning AFTER UPDATE ON items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION probe()",
    ) else {
        panic!("expected CREATE TRIGGER");
    };
    assert!(!statement.row);
    assert_eq!(statement.transition_relations.len(), 2);
    assert_eq!(statement.old_transition_table(), Some("old_rows"));
    assert_eq!(statement.new_transition_table(), Some("new_rows"));

    let Statement::CreateTrigger(row) = first(
        "CREATE TRIGGER transitioning_row AFTER INSERT ON items REFERENCING NEW TABLE AS inserted_rows FOR EACH ROW EXECUTE FUNCTION probe()",
    ) else {
        panic!("expected row CREATE TRIGGER");
    };
    assert!(row.row);
    assert_eq!(row.old_transition_table(), None);
    assert_eq!(row.new_transition_table(), Some("inserted_rows"));
}

#[test]
fn instead_of_trigger_shape_is_preserved_for_execution_validation() {
    let Statement::CreateTrigger(trigger) = first(
        "CREATE TRIGGER view_instead INSTEAD OF INSERT OR UPDATE OR DELETE ON item_view FOR EACH ROW EXECUTE FUNCTION probe()",
    ) else {
        panic!("expected CREATE TRIGGER");
    };
    assert_eq!(trigger.timing, crate::ast::TriggerTiming::InsteadOf);
    assert!(trigger.row);
    assert_eq!(
        trigger.events,
        [
            crate::ast::TriggerEvent::Insert,
            crate::ast::TriggerEvent::Update,
            crate::ast::TriggerEvent::Delete,
        ]
    );
}

#[test]
fn rule_statements_preserve_event_actions_and_lifecycle_shape() {
    let Statement::CreateRule(rule) = first(
        "CREATE OR REPLACE RULE audit_insert AS ON INSERT TO app.items WHERE NEW.id > 0 DO ALSO (INSERT INTO app.audit VALUES (NEW.id); UPDATE app.stats SET n = n + 1;)",
    ) else {
        panic!("expected CREATE RULE");
    };
    assert_eq!(rule.name, "audit_insert");
    assert_eq!(rule.table, "app.items");
    assert_eq!(rule.event, crate::ast::RuleEvent::Insert);
    assert!(!rule.instead);
    assert!(rule.condition.is_some());
    assert_eq!(rule.actions.len(), 2);
    assert_eq!(rule.action_sql.len(), 2);
    assert!(rule.or_replace);

    let Statement::CreateRule(nothing) =
        first("CREATE RULE suppress_delete AS ON DELETE TO app.items DO INSTEAD NOTHING")
    else {
        panic!("expected CREATE RULE DO NOTHING");
    };
    assert!(nothing.instead);
    assert!(nothing.actions.is_empty());
    assert!(nothing.action_sql.is_empty());

    let Statement::DropRule(drop) = first("DROP RULE IF EXISTS audit_insert ON app.items CASCADE")
    else {
        panic!("expected DROP RULE");
    };
    assert_eq!(drop.name, "audit_insert");
    assert_eq!(drop.table, "app.items");
    assert!(drop.if_exists);
    assert!(drop.cascade);

    let Statement::AlterTable(rename) =
        first("ALTER RULE audit_insert ON app.items RENAME TO renamed_audit")
    else {
        panic!("expected ALTER RULE");
    };
    assert!(matches!(
        rename.actions.as_slice(),
        [AlterTableAction::RenameRule { from, to }]
            if from == "audit_insert" && to == "renamed_audit"
    ));

    let Statement::AlterTable(enable) =
        first("ALTER TABLE app.items ENABLE REPLICA RULE renamed_audit")
    else {
        panic!("expected ALTER TABLE ENABLE RULE");
    };
    assert!(matches!(
        enable.actions.as_slice(),
        [AlterTableAction::SetRuleEnableMode { name, mode }]
            if name == "renamed_audit" && *mode == crate::ast::EventEnableMode::Replica
    ));
}

#[test]
fn alter_table_add_key_constraint_preserves_tuple_shape() {
    let Statement::AlterTable(alter) =
        first("ALTER TABLE labels ADD CONSTRAINT labels_tenant_slug_key UNIQUE (tenant, slug)")
    else {
        panic!("expected ALTER TABLE");
    };
    assert!(matches!(
        alter.actions.as_slice(),
        [AlterTableAction::AddKeyConstraint { constraint }]
            if constraint.name.as_deref() == Some("labels_tenant_slug_key")
                && constraint.kind == TableKeyConstraintKind::Unique
                && constraint.columns == ["tenant", "slug"]
    ));
}

#[test]
fn alter_table_hierarchy_preserves_every_parser_field_and_subcommand() {
    let Statement::AlterTable(alter) =
        first("ALTER TABLE ONLY child INHERIT parent_one, NO INHERIT parent_two")
    else {
        panic!("expected ALTER TABLE");
    };
    assert!(!alter.recurse);
    assert!(matches!(
        alter.actions.as_slice(),
        [
            AlterTableAction::AddInheritance { parent: first_parent },
            AlterTableAction::DropInheritance {
                parent: second_parent
            }
        ] if first_parent == "parent_one" && second_parent == "parent_two"
    ));

    let Statement::AlterTable(attach) = first(
        "ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (MINVALUE, 1) TO (10, MAXVALUE)",
    ) else {
        panic!("expected ALTER TABLE");
    };
    assert!(attach.recurse);
    assert!(matches!(
        attach.actions.as_slice(),
        [AlterTableAction::AttachPartition {
            partition,
            bound: crate::ast::PartitionBound::Range { lower, upper },
        }] if partition == "child"
            && matches!(lower.as_slice(), [crate::ast::PartitionRangeDatum::MinValue, crate::ast::PartitionRangeDatum::Value(Expr::Literal(uqa_core::Value::Int(1)))])
            && matches!(upper.as_slice(), [crate::ast::PartitionRangeDatum::Value(Expr::Literal(uqa_core::Value::Int(10))), crate::ast::PartitionRangeDatum::MaxValue])
    ));

    for (sql, concurrently, finalize) in [
        (
            "ALTER TABLE parent DETACH PARTITION child CONCURRENTLY",
            true,
            false,
        ),
        (
            "ALTER TABLE parent DETACH PARTITION child FINALIZE",
            false,
            true,
        ),
    ] {
        let Statement::AlterTable(detach) = first(sql) else {
            panic!("expected ALTER TABLE");
        };
        assert!(matches!(
            detach.actions.as_slice(),
            [AlterTableAction::DetachPartition {
                partition,
                concurrently: actual_concurrently,
                finalize: actual_finalize,
            }] if partition == "child"
                && *actual_concurrently == concurrently
                && *actual_finalize == finalize
        ));
    }
}

#[test]
fn alter_table_constraint_lifecycle_preserves_every_ordered_action() {
    let Statement::AlterTable(alter) = first(
        "ALTER TABLE child \
         ADD CONSTRAINT score_ck CHECK (score > 0) NOT VALID, \
         ADD CONSTRAINT parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED NOT VALID, \
         ADD CONSTRAINT label_nn NOT NULL label NOT VALID NO INHERIT, \
         VALIDATE CONSTRAINT score_ck, \
         ALTER CONSTRAINT parent_fk NOT ENFORCED, \
         ALTER CONSTRAINT parent_fk DEFERRABLE INITIALLY DEFERRED, \
         ALTER CONSTRAINT label_nn NO INHERIT, \
         DROP CONSTRAINT score_ck CASCADE",
    ) else {
        panic!("expected ALTER TABLE");
    };
    assert_eq!(alter.actions.len(), 8);
    assert!(matches!(
        &alter.actions[0],
        AlterTableAction::AddCheckConstraint { constraint }
            if constraint.name.as_deref() == Some("score_ck")
                && constraint.enforced
                && !constraint.validated
    ));
    assert!(matches!(
        &alter.actions[1],
        AlterTableAction::AddForeignKeyConstraint { constraint }
            if constraint.name.as_deref() == Some("parent_fk")
                && constraint.enforced
                && !constraint.validated
                && constraint.deferrable
                && constraint.initially_deferred
    ));
    assert!(matches!(
        &alter.actions[2],
        AlterTableAction::AddNotNullConstraint { name, column, validated, no_inherit }
            if name.as_deref() == Some("label_nn")
                && column == "label"
                && !validated
                && *no_inherit
    ));
    assert!(matches!(
        &alter.actions[3],
        AlterTableAction::ValidateConstraint { name } if name == "score_ck"
    ));
    assert!(matches!(
        &alter.actions[4],
        AlterTableAction::AlterConstraint { name, enforceability: Some(false), .. }
            if name == "parent_fk"
    ));
    assert!(matches!(
        &alter.actions[5],
        AlterTableAction::AlterConstraint {
            name,
            deferrability: Some((true, true)),
            ..
        } if name == "parent_fk"
    ));
    assert!(matches!(
        &alter.actions[6],
        AlterTableAction::AlterConstraint { name, no_inherit: Some(true), .. }
            if name == "label_nn"
    ));
    assert!(matches!(
        &alter.actions[7],
        AlterTableAction::DropConstraint { name, cascade: true, .. } if name == "score_ck"
    ));
}

#[test]
fn set_constraints_preserves_all_named_qualified_and_mode_shapes() {
    let Statement::SetConstraints {
        constraints,
        deferred,
    } = first("SET CONSTRAINTS ALL DEFERRED")
    else {
        panic!("expected SET CONSTRAINTS");
    };
    assert!(constraints.is_empty());
    assert!(deferred);

    let Statement::SetConstraints {
        constraints,
        deferred,
    } = first("SET CONSTRAINTS child_fk, app.\"Mixed FK\" IMMEDIATE")
    else {
        panic!("expected SET CONSTRAINTS");
    };
    assert_eq!(
        constraints,
        [
            crate::ast::SetConstraintName {
                catalog: None,
                schema: None,
                name: "child_fk".into(),
            },
            crate::ast::SetConstraintName {
                catalog: None,
                schema: Some("app".into()),
                name: "Mixed FK".into(),
            },
        ]
    );
    assert!(!deferred);
}

#[test]
fn named_table_not_null_can_replace_implicit_column_nullability() {
    for sql in [
        "CREATE TABLE keyed (id INTEGER PRIMARY KEY, CONSTRAINT keyed_id_nn NOT NULL id)",
        "CREATE TABLE serial_key (id SERIAL, CONSTRAINT serial_id_nn NOT NULL id)",
        "CREATE TABLE bigserial_key (id BIGSERIAL, CONSTRAINT bigserial_id_nn NOT NULL id)",
        "CREATE TABLE identity_key (id INTEGER GENERATED BY DEFAULT AS IDENTITY, CONSTRAINT identity_id_nn NOT NULL id)",
    ] {
        let Statement::CreateTable(table) = first(sql) else {
            panic!("expected CREATE TABLE for {sql}");
        };
        let column = &table.columns[0];
        assert!(column.not_null, "{sql}");
        assert!(column.not_null_explicit, "{sql}");
        assert!(column.not_null_name.is_some(), "{sql}");
    }
}

#[test]
fn omitted_foreign_key_columns_are_preserved_for_primary_key_inference() {
    let Statement::CreateTable(table) =
        first("CREATE TABLE child (parent_id INTEGER REFERENCES parent)")
    else {
        panic!("expected CREATE TABLE");
    };
    assert!(table.columns[0]
        .references
        .as_ref()
        .is_some_and(|reference| reference.column.is_none()));

    let Statement::AlterTable(alter) = first(
        "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent",
    ) else {
        panic!("expected ALTER TABLE");
    };
    assert!(matches!(
        alter.actions.as_slice(),
        [AlterTableAction::AddForeignKeyConstraint { constraint }]
            if constraint.ref_columns.is_empty()
    ));
}

#[test]
fn not_enforced_constraints_start_unvalidated() {
    let Statement::CreateTable(table) = first(
        "CREATE TABLE child (score INTEGER CHECK (score > 0) NOT ENFORCED, parent_id INTEGER REFERENCES parent(id) NOT ENFORCED, alt_id INTEGER, CHECK (alt_id > 0) NOT ENFORCED, FOREIGN KEY (alt_id) REFERENCES parent(id) NOT ENFORCED)",
    ) else {
        panic!("expected CREATE TABLE");
    };
    assert!(!table.columns[0].check_enforced);
    assert!(!table.columns[0].check_validated);
    let column_reference = table.columns[1].references.as_ref().unwrap();
    assert!(!column_reference.enforced);
    assert!(!column_reference.validated);
    assert!(!table.checks[0].enforced);
    assert!(!table.checks[0].validated);
    assert!(!table.foreign_keys[0].enforced);
    assert!(!table.foreign_keys[0].validated);
}

#[test]
fn alter_constraint_not_valid_reports_postgresql_feature_state() {
    let error = compile("ALTER TABLE child ALTER CONSTRAINT parent_fk NOT VALID").unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert!(error
        .to_string()
        .contains("constraints cannot be altered to be NOT VALID"));
}

#[test]
fn alter_sequence_preserves_if_exists() {
    let Statement::AlterSequence(sequence) =
        first("ALTER SEQUENCE IF EXISTS absent AS smallint INCREMENT -2 MINVALUE -20 MAXVALUE -2 START -4 RESTART WITH -6 CACHE 7 CYCLE")
    else {
        panic!("expected ALTER SEQUENCE");
    };
    assert!(sequence.if_exists);
    assert_eq!(
        sequence.data_type,
        Some(crate::ast::SequenceDataType::SmallInt)
    );
    assert_eq!(sequence.increment, Some(-2));
    assert_eq!(sequence.min_value, crate::ast::SequenceBound::Value(-20));
    assert_eq!(sequence.max_value, crate::ast::SequenceBound::Value(-2));
    assert_eq!(sequence.start, Some(-4));
    assert_eq!(sequence.restart, crate::ast::SequenceRestart::With(-6));
    assert_eq!(sequence.cycle, Some(true));
    assert_eq!(sequence.cache_size, Some(7));

    let Statement::AlterSequence(defaults) =
        first("ALTER SEQUENCE absent NO MINVALUE NO MAXVALUE NO CYCLE RESTART")
    else {
        panic!("expected ALTER SEQUENCE defaults");
    };
    assert_eq!(defaults.min_value, crate::ast::SequenceBound::Default);
    assert_eq!(defaults.max_value, crate::ast::SequenceBound::Default);
    assert_eq!(defaults.cycle, Some(false));
    assert_eq!(defaults.restart, crate::ast::SequenceRestart::FromStart);
}

#[test]
fn alter_sequence_persistence_preserves_target_and_historical_table_syntax() {
    let Statement::AlterSequence(unlogged) = first("ALTER SEQUENCE IF EXISTS app.ids SET UNLOGGED")
    else {
        panic!("expected ALTER SEQUENCE SET UNLOGGED");
    };
    assert_eq!(unlogged.name, "app.ids");
    assert!(unlogged.if_exists);
    assert_eq!(
        unlogged.persistence,
        Some(crate::ast::RelationPersistence::Unlogged)
    );

    let Statement::AlterSequence(logged) = first("ALTER SEQUENCE app.ids SET LOGGED") else {
        panic!("expected ALTER SEQUENCE SET LOGGED");
    };
    assert_eq!(
        logged.persistence,
        Some(crate::ast::RelationPersistence::Permanent)
    );

    let Statement::AlterTable(table_syntax) = first("ALTER TABLE app.ids SET UNLOGGED") else {
        panic!("expected historical ALTER TABLE sequence syntax");
    };
    assert!(matches!(
        table_syntax.actions.as_slice(),
        [crate::ast::AlterTableAction::SetPersistence {
            persistence: crate::ast::RelationPersistence::Unlogged
        }]
    ));
}

#[test]
fn alter_sequence_name_lifecycle_preserves_direct_and_historical_syntax() {
    let Statement::AlterSequence(rename) =
        first("ALTER SEQUENCE IF EXISTS app.ids RENAME TO renamed_ids")
    else {
        panic!("expected ALTER SEQUENCE RENAME TO");
    };
    assert_eq!(rename.name, "app.ids");
    assert!(rename.if_exists);
    assert_eq!(
        rename.lifecycle,
        crate::ast::SequenceLifecycle::RenameTo {
            name: "renamed_ids".into()
        }
    );

    let Statement::AlterSequence(set_schema) =
        first("ALTER SEQUENCE IF EXISTS app.ids SET SCHEMA archive")
    else {
        panic!("expected ALTER SEQUENCE SET SCHEMA");
    };
    assert!(set_schema.if_exists);
    assert_eq!(
        set_schema.lifecycle,
        crate::ast::SequenceLifecycle::SetSchema {
            schema: "archive".into()
        }
    );

    let Statement::AlterTable(table_rename) = first("ALTER TABLE app.ids RENAME TO renamed_ids")
    else {
        panic!("expected historical ALTER TABLE RENAME TO");
    };
    assert!(matches!(
        table_rename.actions.as_slice(),
        [crate::ast::AlterTableAction::RenameTable { to }] if to == "renamed_ids"
    ));

    let Statement::AlterTable(table_schema) =
        first("ALTER TABLE IF EXISTS app.ids SET SCHEMA archive")
    else {
        panic!("expected historical ALTER TABLE SET SCHEMA");
    };
    assert!(table_schema.if_exists);
    assert!(matches!(
        table_schema.actions.as_slice(),
        [crate::ast::AlterTableAction::SetSchema { schema }] if schema == "archive"
    ));
}

#[test]
fn create_table_as_preserves_positional_column_names() {
    let Statement::CreateTableAs {
        name,
        if_not_exists,
        column_names,
        with_no_data,
        ..
    } = first(
        "CREATE TABLE IF NOT EXISTS app.copy (renamed, \"Mixed\") AS \
         SELECT 1, 2, 3 WITH NO DATA",
    )
    else {
        panic!("expected CREATE TABLE AS");
    };
    assert_eq!(name, "app.copy");
    assert!(if_not_exists);
    assert_eq!(column_names, ["renamed", "Mixed"]);
    assert!(with_no_data);

    let Statement::CreateTableAs { with_no_data, .. } =
        first("CREATE TABLE populated AS SELECT 1 WITH DATA")
    else {
        panic!("expected CREATE TABLE AS");
    };
    assert!(!with_no_data);
}

#[test]
fn select_into_lowers_to_the_create_table_as_contract() {
    let Statement::CreateTableAs {
        name,
        if_not_exists,
        column_names,
        with_no_data,
        body,
        ..
    } = first("SELECT 1::smallint AS value INTO app.\"Copied\"")
    else {
        panic!("expected SELECT INTO to lower as CREATE TABLE AS");
    };
    assert_eq!(name, "app.\"Copied\"");
    assert!(!if_not_exists);
    assert!(column_names.is_empty());
    assert!(!with_no_data);
    assert_eq!(body.projections.len(), 1);
    assert_eq!(body.projections[0].alias.as_deref(), Some("value"));

    let Statement::CreateTableAs { name, body, .. } = first(
        "SELECT 1 AS value INTO union_copy \
         UNION ALL SELECT 2",
    ) else {
        panic!("expected set-operation SELECT INTO");
    };
    assert_eq!(name, "union_copy");
    assert!(body.set_op.is_some());

    let Statement::Prepare { body, .. } = first(
        "PREPARE make_copy AS \
         SELECT 7::smallint AS value INTO prepared_copy",
    ) else {
        panic!("expected PREPARE");
    };
    assert!(matches!(*body, Statement::CreateTableAs { .. }));
}

#[test]
fn create_view_preserves_positional_column_names() {
    let Statement::CreateView {
        name,
        column_names,
        or_replace,
        ..
    } = first("CREATE OR REPLACE VIEW app.labels (renamed, \"Mixed.Name\") AS SELECT 1, 2, 3")
    else {
        panic!("expected CREATE VIEW");
    };
    assert_eq!(name, "app.labels");
    assert_eq!(column_names, ["renamed", "Mixed.Name"]);
    assert!(or_replace);
}

#[test]
fn direct_unknown_literal_casts_are_validated_during_analysis() {
    let error = compile("SELECT 'bad'::integer").unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P02"));

    compile("SELECT ('bad'::text)::integer").unwrap();
    compile("SELECT 999999999999::integer").unwrap();
}

#[test]
fn relation_forms_and_options_preserve_lifecycle_semantics() {
    let Statement::CreateTable(temporary) =
        first("CREATE TEMP TABLE temp_t (id INTEGER) ON COMMIT DELETE ROWS")
    else {
        panic!("expected CREATE TEMP TABLE");
    };
    assert_eq!(
        temporary.persistence,
        crate::ast::RelationPersistence::Temporary
    );
    assert_eq!(temporary.on_commit, crate::ast::OnCommitAction::DeleteRows);

    let Statement::CreateTable(unlogged) = first("CREATE UNLOGGED TABLE unlogged_t (id INTEGER)")
    else {
        panic!("expected CREATE UNLOGGED TABLE");
    };
    assert_eq!(
        unlogged.persistence,
        crate::ast::RelationPersistence::Unlogged
    );

    let Statement::CreateView {
        persistence,
        options,
        ..
    } = first(
        "CREATE TEMP VIEW temp_v WITH (security_barrier=true) AS SELECT 1 WITH LOCAL CHECK OPTION",
    )
    else {
        panic!("expected CREATE TEMP VIEW");
    };
    assert_eq!(persistence, crate::ast::RelationPersistence::Temporary);
    assert_eq!(
        options,
        [
            ("security_barrier".into(), "true".into()),
            ("check_option".into(), "local".into()),
        ]
    );

    assert!(matches!(
        first("CREATE MATERIALIZED VIEW materialized WITH (fillfactor=80) AS SELECT 1 WITH NO DATA"),
        Statement::CreateMaterializedView { with_no_data: true, options, .. }
            if options == [("fillfactor".into(), "80".into())]
    ));
    assert!(matches!(
        first("CREATE TEMP TABLE temp_as AS SELECT 1"),
        Statement::CreateTableAs {
            persistence: crate::ast::RelationPersistence::Temporary,
            ..
        }
    ));
    assert!(matches!(
        first("CREATE TEMP SEQUENCE temp_sequence"),
        Statement::CreateSequence(crate::ast::CreateSequence {
            persistence: crate::ast::RelationPersistence::Temporary,
            ..
        })
    ));
    assert!(matches!(
        first("ALTER VIEW temp_v SET (security_invoker=on)"),
        Statement::AlterView(crate::ast::AlterViewStmt {
            kind: crate::ast::AlterViewKind::View,
            action: crate::ast::AlterViewAction::Set(options),
            ..
        }) if options == [("security_invoker".into(), "on".into())]
    ));
    assert!(matches!(
        first("ALTER VIEW temp_v RESET (security_barrier)"),
        Statement::AlterView(crate::ast::AlterViewStmt {
            action: crate::ast::AlterViewAction::Reset(options),
            ..
        }) if options == ["security_barrier"]
    ));
    assert!(matches!(
        first("ALTER VIEW temp_v OWNER TO next_owner"),
        Statement::AlterView(crate::ast::AlterViewStmt {
            kind: crate::ast::AlterViewKind::View,
            action: crate::ast::AlterViewAction::OwnerTo(owner),
            ..
        }) if owner == "next_owner"
    ));
    assert!(matches!(
        first("ALTER MATERIALIZED VIEW reports OWNER TO CURRENT_USER"),
        Statement::AlterView(crate::ast::AlterViewStmt {
            kind: crate::ast::AlterViewKind::MaterializedView,
            action: crate::ast::AlterViewAction::OwnerTo(owner),
            ..
        }) if owner == "CURRENT_USER"
    ));
    assert!(matches!(
        compile("ALTER VIEW temp_v RENAME TO renamed").unwrap_err(),
        SQLError::Unsupported(_)
    ));
}

#[test]
fn unsupported_create_ddl_never_loses_remaining_envelope_semantics() {
    for (sql, expected) in [
        (
            "CREATE TABLE optioned (id INTEGER) WITH (fillfactor = 70)",
            "storage options",
        ),
        (
            "CREATE TABLE spaced (id INTEGER) TABLESPACE fastspace",
            "TABLESPACE",
        ),
        (
            "CREATE TABLE accessed (id INTEGER) USING heap",
            "access methods",
        ),
        (
            "CREATE SCHEMA owned AUTHORIZATION CURRENT_USER",
            "AUTHORIZATION",
        ),
        (
            "CREATE SCHEMA bundled CREATE TABLE child (id INTEGER)",
            "schema elements",
        ),
        (
            "CREATE UNLOGGED VIEW unlogged_v AS SELECT 1",
            "cannot be unlogged",
        ),
        (
            "CREATE TEMP MATERIALIZED VIEW materialized AS SELECT 1",
            "syntax error",
        ),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {sql}: {error}"
        );
    }
}
