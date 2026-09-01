//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema definition, declared type, routine type, and key-constraint compilation.

use super::*;

#[test]
fn sequence_options_do_not_truncate_or_ignore_values() {
    let Statement::CreateSequence(sequence) = first(
        "CREATE SEQUENCE app.s AS integer INCREMENT BY 3 MINVALUE 2 MAXVALUE 10 START WITH 8 CACHE 4 CYCLE",
    ) else {
        panic!("not CREATE SEQUENCE");
    };
    assert_eq!(sequence.data_type, crate::ast::SequenceDataType::Integer);
    assert_eq!(sequence.increment, 3);
    assert_eq!(sequence.min_value, Some(2));
    assert_eq!(sequence.max_value, Some(10));
    assert_eq!(sequence.start, 8);
    assert!(sequence.cycle);
    assert_eq!(sequence.cache_size, 4);

    let Statement::CreateSequence(descending) = first(
        "CREATE SEQUENCE descending AS smallint INCREMENT -2 NO MINVALUE NO MAXVALUE NO CYCLE",
    ) else {
        panic!("not descending CREATE SEQUENCE");
    };
    assert_eq!(descending.data_type, crate::ast::SequenceDataType::SmallInt);
    assert_eq!(descending.min_value, Some(i64::from(i16::MIN)));
    assert_eq!(descending.max_value, Some(-1));
    assert_eq!(descending.start, -1);
    assert!(!descending.cycle);

    assert_eq!(
        compile("CREATE SEQUENCE s START 1.5")
            .unwrap_err()
            .sqlstate(),
        Some("22P02")
    );
    assert_eq!(
        compile("CREATE SEQUENCE s START 9223372036854775808")
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    assert_eq!(
        compile("CREATE SEQUENCE s MINVALUE 1 MINVALUE 2")
            .unwrap_err()
            .sqlstate(),
        Some("42601")
    );
    let Statement::CreateSequence(cached) = first("CREATE SEQUENCE s CACHE 10") else {
        panic!("not cached CREATE SEQUENCE");
    };
    assert_eq!(cached.cache_size, 10);
}

#[test]
fn sequence_ownership_preserves_target_names_and_none_actions() {
    let Statement::CreateSequence(sequence) =
        first("CREATE SEQUENCE app.s OWNED BY app.\"Owner\".\"Mixed\"")
    else {
        panic!("not CREATE SEQUENCE");
    };
    assert_eq!(
        sequence.ownership,
        crate::ast::SequenceOwnership::Column {
            table: "app.\"Owner\"".into(),
            column: "Mixed".into(),
        }
    );

    let Statement::CreateSequence(unowned) = first("CREATE SEQUENCE free_ids OWNED BY NONE") else {
        panic!("not unowned CREATE SEQUENCE");
    };
    assert_eq!(unowned.ownership, crate::ast::SequenceOwnership::Unowned);

    let Statement::AlterSequence(alter) = first("ALTER SEQUENCE s OWNED BY owner_table.id") else {
        panic!("not ALTER SEQUENCE");
    };
    assert_eq!(
        alter.ownership,
        crate::ast::SequenceOwnership::Column {
            table: "owner_table".into(),
            column: "id".into(),
        }
    );
    let Statement::AlterSequence(detach) = first("ALTER SEQUENCE s OWNED BY NONE") else {
        panic!("not detached ALTER SEQUENCE");
    };
    assert_eq!(detach.ownership, crate::ast::SequenceOwnership::Unowned);

    assert_eq!(
        compile("CREATE SEQUENCE invalid_owner OWNED BY owner_table")
            .unwrap_err()
            .sqlstate(),
        Some("42601")
    );
    assert_eq!(
        compile("CREATE SEQUENCE duplicate_owner OWNED BY owner_table.id OWNED BY NONE")
            .unwrap_err()
            .sqlstate(),
        Some("42601")
    );
    assert_eq!(
        compile("CREATE SEQUENCE cross_database OWNED BY database.app.owner_table.id")
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
}

#[test]
fn sequence_role_owner_preserves_direct_and_historical_syntax() {
    let Statement::AlterSequence(direct) = first("ALTER SEQUENCE app.ids OWNER TO next_owner")
    else {
        panic!("not ALTER SEQUENCE");
    };
    assert_eq!(direct.name, "app.ids");
    assert_eq!(direct.role_owner.as_deref(), Some("next_owner"));

    let Statement::AlterTable(historical) = first("ALTER TABLE app.ids OWNER TO CURRENT_USER")
    else {
        panic!("not ALTER TABLE");
    };
    assert!(matches!(
        historical.actions.as_slice(),
        [crate::ast::AlterTableAction::ChangeOwner { owner }] if owner == "CURRENT_USER"
    ));

    let Statement::AlterSequence(public) = first("ALTER SEQUENCE ids OWNER TO PUBLIC") else {
        panic!("not ALTER SEQUENCE");
    };
    assert_eq!(public.role_owner.as_deref(), Some("public"));
}

#[test]
fn sequence_grants_preserve_privileges_targets_and_grant_paths() {
    use crate::ast::{GrantSequenceTarget, SequencePrivilege, SequenceRevokeBehavior};

    let Statement::GrantSequence(grant) = first(
        "GRANT USAGE, SELECT ON SEQUENCE app.ids, ids2 TO caller, PUBLIC WITH GRANT OPTION GRANTED BY CURRENT_USER",
    ) else {
        panic!("not sequence GRANT");
    };
    assert!(grant.is_grant);
    assert!(grant.grant_option);
    assert_eq!(
        grant.privileges,
        vec![SequencePrivilege::Usage, SequencePrivilege::Select]
    );
    assert!(matches!(
        grant.target,
        GrantSequenceTarget::Sequences {
            ref names,
            require_sequence: true,
        } if names == &["app.ids", "ids2"]
    ));
    assert_eq!(grant.grantees, ["caller", "PUBLIC"]);
    assert_eq!(grant.grantor.as_deref(), Some("CURRENT_USER"));

    let Statement::GrantSequence(revoke) = first(
        "REVOKE GRANT OPTION FOR ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA app, public FROM caller CASCADE",
    ) else {
        panic!("not sequence REVOKE");
    };
    assert!(!revoke.is_grant);
    assert!(revoke.grant_option_only);
    assert_eq!(revoke.revoke_behavior, SequenceRevokeBehavior::Cascade);
    assert_eq!(
        revoke.privileges,
        vec![
            SequencePrivilege::Select,
            SequencePrivilege::Update,
            SequencePrivilege::Usage,
        ]
    );
    assert!(matches!(
        revoke.target,
        GrantSequenceTarget::AllSequencesInSchemas { ref schemas }
            if schemas == &["app", "public"]
    ));

    let Statement::GrantSequence(historical) = first("GRANT SELECT ON TABLE app.ids TO caller")
    else {
        panic!("not historical sequence-compatible GRANT");
    };
    assert!(matches!(
        historical.target,
        GrantSequenceTarget::Sequences {
            require_sequence: false,
            ..
        }
    ));

    let Statement::GrantSequence(invalid) = first("GRANT INSERT ON SEQUENCE ids TO caller") else {
        panic!("not deferred invalid sequence privilege");
    };
    assert_eq!(
        invalid.privileges,
        vec![SequencePrivilege::Unsupported("INSERT".into())]
    );

    let Statement::GrantSequence(columns) = first("GRANT SELECT (value) ON SEQUENCE ids TO caller")
    else {
        panic!("not deferred sequence column privilege");
    };
    assert_eq!(
        columns.privileges,
        vec![SequencePrivilege::ColumnsUnsupported]
    );
}

#[test]
fn create_table_with_vector_column() {
    let stmt = first("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, embedding VECTOR(4))");
    let Statement::CreateTable(ct) = stmt else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(ct.name, "docs");
    assert_eq!(ct.columns.len(), 3);
    assert!(matches!(ct.columns[0].ty, ColumnType::Integer));
    assert!(ct.columns[0].primary_key);
    assert!(matches!(ct.columns[1].ty, ColumnType::Text));
    assert!(matches!(ct.columns[2].ty, ColumnType::Vector(4)));
}

#[test]
fn create_table_preserves_boolean_column_type() {
    let Statement::CreateTable(table) = first("CREATE TABLE flags (enabled BOOLEAN)") else {
        panic!("not CREATE TABLE");
    };
    assert!(matches!(table.columns[0].ty, ColumnType::Boolean));
}

#[test]
fn create_table_preserves_fixed_character_length() {
    let Statement::CreateTable(table) = first("CREATE TABLE labels (code CHAR(7))") else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(table.columns[0].ty, ColumnType::Character(7));
}

#[test]
fn create_table_preserves_postgresql_scalar_type_identity() {
    let Statement::CreateTable(table) = first(
        "CREATE TABLE typed_values (
            small_value SMALLINT,
            integer_value INTEGER,
            big_value BIGINT,
            oid_value OID,
            xid_value XID,
            real_value REAL,
            double_value DOUBLE PRECISION,
            text_value TEXT,
            name_value NAME,
            uuid_value UUID,
            varying_value VARCHAR(12),
            interval_value INTERVAL
        )",
    ) else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(
        table
            .columns
            .iter()
            .map(|column| column.ty.clone())
            .collect::<Vec<_>>(),
        vec![
            ColumnType::SmallInteger,
            ColumnType::Integer,
            ColumnType::BigInteger,
            ColumnType::Oid,
            ColumnType::Xid,
            ColumnType::Real,
            ColumnType::DoublePrecision,
            ColumnType::Text,
            ColumnType::Name,
            ColumnType::Uuid,
            ColumnType::Varchar(Some(12)),
            ColumnType::Interval,
        ]
    );
}

#[test]
fn serial_family_preserves_width_and_sequence_semantics() {
    let Statement::CreateTable(table) =
        first("CREATE TABLE generated_ids (small_id SMALLSERIAL, id SERIAL4, big_id SERIAL8)")
    else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(
        table
            .columns
            .iter()
            .map(|column| {
                (
                    column.ty.clone(),
                    column.auto_increment.as_ref().map(|value| value.kind),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                ColumnType::SmallInteger,
                Some(crate::ast::AutoIncrementKind::Serial),
            ),
            (
                ColumnType::Integer,
                Some(crate::ast::AutoIncrementKind::Serial),
            ),
            (
                ColumnType::BigInteger,
                Some(crate::ast::AutoIncrementKind::Serial),
            ),
        ]
    );
}

#[test]
fn identity_generation_provenance_is_not_serial() {
    let Statement::CreateTable(table) = first(
        "CREATE TABLE generated_ids (always_id INTEGER GENERATED ALWAYS AS IDENTITY, default_id BIGINT GENERATED BY DEFAULT AS IDENTITY)",
    ) else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(
        table.columns[0]
            .auto_increment
            .as_ref()
            .map(|value| value.kind),
        Some(crate::ast::AutoIncrementKind::IdentityAlways)
    );
    assert_eq!(
        table.columns[1]
            .auto_increment
            .as_ref()
            .map(|value| value.kind),
        Some(crate::ast::AutoIncrementKind::IdentityByDefault)
    );
}

#[test]
fn create_table_preserves_array_element_types_and_dimensions() {
    let Statement::CreateTable(table) =
        first("CREATE TABLE arrays (tags TEXT[], matrix INTEGER[][])")
    else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(
        table.columns[0].ty,
        ColumnType::Array(Box::new(ColumnType::Text))
    );
    assert_eq!(
        table.columns[1].ty,
        ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Integer))))
    );
}

#[test]
fn routine_type_names_preserve_percent_type_and_named_type_qualification() {
    let Statement::CreateFunction(function) = first(
        "CREATE FUNCTION typed_value(v app.items.value%TYPE, d app.amount_domain)
         RETURNS app.items.id%TYPE LANGUAGE sql AS $$ SELECT 1 $$",
    ) else {
        panic!("not CREATE FUNCTION");
    };
    assert_eq!(function.params[0].type_name, "app.items.value%type");
    assert_eq!(
        function.params[0].type_reference,
        Some(crate::ast::RoutineColumnTypeReference::new(
            Some("app".into()),
            "items".into(),
            "value".into()
        ))
    );
    assert_eq!(function.params[1].type_name, "app.amount_domain");
    assert!(matches!(
        function.returns,
        crate::ast::FunctionReturns::Scalar { type_name }
            if type_name == "app.items.id%type"
    ));
}

#[test]
fn routine_builtin_array_names_use_sql_array_spelling() {
    let Statement::CreateFunction(function) = first(
        "CREATE FUNCTION array_names(integer[]) RETURNS text[] LANGUAGE sql AS $$ SELECT ARRAY['x'] $$",
    ) else {
        panic!("not CREATE FUNCTION");
    };
    assert_eq!(function.params[0].type_name, "int4[]");
    assert!(matches!(
        function.returns,
        crate::ast::FunctionReturns::Scalar { type_name } if type_name == "text[]"
    ));
}

#[test]
fn routine_percent_type_keeps_quoted_dotted_components_structured() {
    let Statement::CreateFunction(function) = first(
        "CREATE FUNCTION typed_dot(v \"app.dot\".\"items.dot\".\"value.dot\"%TYPE)
         RETURNS \"app.dot\".\"items.dot\".\"value.dot\"%TYPE LANGUAGE sql AS $$ SELECT $1 $$",
    ) else {
        panic!("not CREATE FUNCTION");
    };
    let expected = crate::ast::RoutineColumnTypeReference::new(
        Some("app.dot".into()),
        "items.dot".into(),
        "value.dot".into(),
    );
    assert_eq!(function.params[0].type_reference, Some(expected.clone()));
    assert_eq!(function.return_type_reference, Some(expected));
    assert_eq!(
        function.params[0].type_name,
        "\"app.dot\".\"items.dot\".\"value.dot\"%type"
    );
}

#[test]
fn create_table_preserves_typed_composite_keys_and_null_policy() {
    let Statement::CreateTable(table) = first(
        "CREATE TABLE memberships (
            tenant TEXT,
            member TEXT,
            email TEXT,
            CONSTRAINT memberships_pkey PRIMARY KEY (tenant, member),
            CONSTRAINT memberships_email_key UNIQUE NULLS NOT DISTINCT (tenant, email)
        )",
    ) else {
        panic!("not CREATE TABLE");
    };

    assert_eq!(table.key_constraints.len(), 2);
    assert_eq!(
        table.key_constraints[0].kind,
        TableKeyConstraintKind::PrimaryKey
    );
    assert_eq!(table.key_constraints[0].columns, vec!["tenant", "member"]);
    assert_eq!(
        table.key_constraints[0].name.as_deref(),
        Some("memberships_pkey")
    );
    assert_eq!(
        table.key_constraints[1].kind,
        TableKeyConstraintKind::Unique
    );
    assert_eq!(table.key_constraints[1].columns, vec!["tenant", "email"]);
    assert!(table.key_constraints[1].nulls_not_distinct);

    assert!(table.columns[0].not_null);
    assert!(table.columns[1].not_null);
    assert!(!table.columns[0].primary_key);
    assert!(!table.columns[1].primary_key);
}

#[test]
fn create_table_preserves_named_column_keys() {
    let Statement::CreateTable(table) = first(
        "CREATE TABLE users (
            id INTEGER CONSTRAINT users_pkey PRIMARY KEY,
            email TEXT CONSTRAINT users_email_key UNIQUE
        )",
    ) else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(table.key_constraints.len(), 2);
    assert_eq!(table.key_constraints[0].name.as_deref(), Some("users_pkey"));
    assert_eq!(
        table.key_constraints[1].name.as_deref(),
        Some("users_email_key")
    );
    assert!(table.columns[0].not_null);
}

#[test]
fn create_table_rejects_invalid_key_declarations() {
    for sql in [
        "CREATE TABLE t (a INTEGER, CONSTRAINT same UNIQUE (a), CONSTRAINT same CHECK (a > 0))",
        "CREATE TABLE t (a INTEGER, UNIQUE (missing))",
        "CREATE TABLE t (a INTEGER, UNIQUE (a, a))",
        "CREATE TABLE t (a INTEGER PRIMARY KEY, b INTEGER, PRIMARY KEY (b))",
    ] {
        assert!(compile(sql).is_err(), "expected invalid DDL to fail: {sql}");
    }
}

#[test]
fn hash_partition_ast_preserves_keys_and_validated_bounds() {
    let Statement::CreateTable(parent) =
        first("CREATE TABLE hash_parent (id BIGINT, label TEXT) PARTITION BY HASH (id, label)")
    else {
        panic!("not CREATE TABLE");
    };
    let spec = parent
        .hierarchy
        .partition_spec
        .as_ref()
        .expect("partition specification");
    assert_eq!(spec.strategy, crate::ast::PartitionStrategy::Hash);
    assert!(matches!(
        spec.keys.as_slice(),
        [Expr::Column(id), Expr::Column(label)] if id == "id" && label == "label"
    ));

    let Statement::CreateTable(child) = first(
        "CREATE TABLE hash_child PARTITION OF hash_parent FOR VALUES WITH (REMAINDER 3, MODULUS 17)",
    ) else {
        panic!("not CREATE TABLE");
    };
    assert!(matches!(
        child.hierarchy.partition_bound,
        Some(crate::ast::PartitionBound::Hash {
            modulus: 17,
            remainder: 3,
        })
    ));
}

#[test]
fn hash_partition_bound_validation_matches_postgresql_error_order() {
    let zero_modulus =
        compile("CREATE TABLE child PARTITION OF parent FOR VALUES WITH (MODULUS 0, REMAINDER 0)")
            .unwrap_err();
    assert_eq!(zero_modulus.sqlstate(), Some("42P16"));
    assert_eq!(
        zero_modulus.to_string(),
        "modulus for hash partition must be an integer value greater than zero"
    );

    let large_remainder = compile(
        "CREATE TABLE child PARTITION OF parent FOR VALUES WITH (MODULUS 17, REMAINDER 17)",
    )
    .unwrap_err();
    assert_eq!(large_remainder.sqlstate(), Some("42P16"));
    assert_eq!(
        large_remainder.to_string(),
        "remainder for hash partition must be less than modulus"
    );
}

#[test]
fn partition_keys_reject_unimplemented_collations_and_operator_classes() {
    let collation =
        compile("CREATE TABLE hash_text (value TEXT) PARTITION BY HASH (value COLLATE \"C\")")
            .unwrap_err();
    assert_eq!(collation.sqlstate(), Some("0A000"));
    assert!(collation
        .to_string()
        .contains("non-default partition key collations"));

    let opclass =
        compile("CREATE TABLE hash_integer (value INTEGER) PARTITION BY HASH (value int4_ops)")
            .unwrap_err();
    assert_eq!(opclass.sqlstate(), Some("0A000"));
    assert!(opclass
        .to_string()
        .contains("partition key operator classes"));
}
