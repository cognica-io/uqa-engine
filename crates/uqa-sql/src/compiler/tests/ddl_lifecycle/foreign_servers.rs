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
