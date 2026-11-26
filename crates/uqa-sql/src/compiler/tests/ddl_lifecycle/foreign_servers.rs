//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn foreign_server_strings_preserve_absent_empty_and_null_values() {
    for (clause, ty, version) in [
        ("", None, None),
        ("TYPE ''", Some(""), None),
        ("VERSION ''", None, Some("")),
        ("VERSION NULL", None, None),
        (
            "TYPE 'TYPE' VERSION 'VERSION'",
            Some("TYPE"),
            Some("VERSION"),
        ),
        ("TYPE $$a$$ VERSION $$b$$", Some("a"), Some("b")),
        (
            "TYPE /* VERSION 'ignored' */ '' VERSION -- TYPE 'ignored'\n ''",
            Some(""),
            Some(""),
        ),
    ] {
        for conditional in ["", "IF NOT EXISTS "] {
            let sql = format!("-- prefix\n CREATE SERVER {conditional}\"TYPE\" {clause} FOREIGN DATA WRAPPER memory_fdw OPTIONS (version 'unrelated')");
            let Statement::CreateForeignServer(server) = first(&sql) else {
                panic!("{sql}");
            };
            assert_eq!(server.server_type.as_deref(), ty, "{sql}");
            assert_eq!(server.version.as_deref(), version, "{sql}");
            assert_eq!(server.options, [("version".into(), "unrelated".into())]);
        }
    }
}

#[test]
fn legacy_foreign_server_ast_reads_without_declaration_strings() {
    let server: crate::ast::CreateForeignServer = serde_json::from_value(serde_json::json!({
        "name":"source", "fdw_type":"memory_fdw", "options":[], "if_not_exists":false
    }))
    .unwrap();
    assert_eq!(server.server_type, None);
    assert_eq!(server.version, None);
}

#[test]
fn foreign_server_unicode_escape_clauses_preserve_empty_declaration_strings() {
    for sql in [
        r#"CREATE SERVER U&"escaped^005fname" UESCAPE '^' TYPE '' VERSION '' FOREIGN DATA WRAPPER memory_fdw"#,
        "CREATE SERVER unicode_type TYPE U&'' UESCAPE '^' VERSION '' FOREIGN DATA WRAPPER memory_fdw",
        r#"CREATE SERVER IF NOT EXISTS U&"escaped^005fname" UESCAPE '^' TYPE U&'' UESCAPE '^' VERSION U&'' UESCAPE '^' FOREIGN DATA WRAPPER memory_fdw"#,
    ] {
        let Statement::CreateForeignServer(server) = first(sql) else { panic!("{sql}"); };
        assert_eq!(server.server_type.as_deref(), Some(""), "{sql}");
        assert_eq!(server.version.as_deref(), Some(""), "{sql}");
    }
}

#[test]
fn drop_server_preserves_written_targets_and_uses_its_own_command_tag() {
    for (prefix, suffix, if_exists, cascade) in [
        ("", "", false, false),
        ("IF EXISTS ", " RESTRICT", true, false),
        ("IF EXISTS ", " CASCADE", true, true),
    ] {
        let statement = first(&format!(
            "DROP SERVER {prefix}\"First.Server\", last_server, \"First.Server\"{suffix}"
        ));
        let Statement::Drop(drop) = &statement else {
            panic!("expected DROP SERVER");
        };
        assert_eq!(drop.kind, crate::ast::DropKind::ForeignServer);
        assert_eq!(drop.names, ["First.Server", "last_server", "First.Server"]);
        assert_eq!(drop.if_exists, if_exists);
        assert_eq!(drop.cascade, cascade);
        let plan = crate::plan::UnifiedPlan::lower(statement);
        let mut result = crate::SQLResult::empty();
        crate::result::completion::set_command_completion(&plan, &mut result, false);
        assert_eq!(result.command_tag.as_deref(), Some("DROP SERVER"));
    }
}

#[test]
fn alter_server_retains_version_presence_and_ordered_actions() {
    use crate::ast::{ForeignOptionChange, ForeignServerVersion};
    for (clause, version) in [
        ("", ForeignServerVersion::Unchanged),
        ("VERSION /* '' */ NULL", ForeignServerVersion::Clear),
        (
            "VERSION U&'' UESCAPE '^'",
            ForeignServerVersion::Set(String::new()),
        ),
        ("VERSION 'NULL'", ForeignServerVersion::Set("NULL".into())),
    ] {
        let sql = format!(
            r#"ALTER SERVER U&"remote^005fname" UESCAPE '^' {clause} OPTIONS (x '', SET x 'abc', DROP x, ADD x 'last')"#
        );
        let Statement::AlterForeignServer(server) = first(&sql) else {
            panic!("{sql}");
        };
        assert_eq!(server.name, "remote_name");
        assert_eq!(server.version, version);
        assert_eq!(
            server.options,
            [
                ForeignOptionChange::Add {
                    name: "x".into(),
                    value: String::new()
                },
                ForeignOptionChange::Set {
                    name: "x".into(),
                    value: "abc".into()
                },
                ForeignOptionChange::Drop { name: "x".into() },
                ForeignOptionChange::Add {
                    name: "x".into(),
                    value: "last".into()
                },
            ]
        );
    }
}
