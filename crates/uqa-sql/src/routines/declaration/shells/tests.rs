//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ColumnDef, ColumnType, Statement};
use std::cell::RefCell;

#[derive(Default)]
struct Catalog {
    probes: RefCell<Vec<String>>,
    usage: RefCell<Vec<u32>>,
    notices: RefCell<Vec<SQLNotice>>,
}

impl RoutineTypeCatalog for Catalog {
    fn shell_type_oid(&self, name: &str) -> Result<Option<u32>, SQLError> {
        self.probes.borrow_mut().push(name.into());
        crate::parse_regtype_name(name)?;
        Ok(Some(16384))
    }

    fn require_type_oid_usage(&self, oid: u32) -> Result<(), SQLError> {
        self.usage.borrow_mut().push(oid);
        Ok(())
    }

    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        panic!("shell inspection does not resolve relation columns")
    }

    fn resolve_catalog_column_type(&self, _: &str) -> Option<ColumnType> {
        panic!("shell inspection does not invent a value layout")
    }

    fn resolve_catalog_column_type_name(&self, _: &str) -> Result<ColumnType, SQLError> {
        panic!("shell inspection does not invent a value layout")
    }

    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        panic!("shell inspection does not invent a value layout")
    }

    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        panic!("shell usage belongs to the undefined identity")
    }

    fn format_type(&self, _: &ColumnType) -> Result<String, SQLError> {
        panic!("shell diagnostics have no value layout")
    }
}

impl RoutineParserCatalog for Catalog {
    fn plpgsql_catalog(&self) -> Result<crate::plpgsql::PlpgsqlCatalog, SQLError> {
        panic!("shell inspection does not compile a routine body")
    }

    fn parser_notice(&self, notice: SQLNotice) {
        self.notices.borrow_mut().push(notice);
    }
}

#[test]
fn routine_column_references_do_not_probe_standalone_shell_names() {
    let catalog = Catalog::default();
    for source in ["source.value%TYPE", "\"s.dot\".\"t.dot\".\"v.dot\"%TYPE"] {
        let Statement::CreateFunction(definition) = crate::compile(&format!(
            "CREATE FUNCTION echo(value {source}) RETURNS {source} LANGUAGE sql AS 'SELECT value'"
        ))
        .unwrap()
        .remove(0) else {
            panic!("expected function declaration")
        };
        for (name, reference, result) in [
            (
                definition.params[0].type_name.as_str(),
                definition.params[0].type_reference.as_ref(),
                false,
            ),
            (
                super::super::result_type_name(&definition),
                definition.return_type_reference.as_ref(),
                true,
            ),
        ] {
            assert!(reference.is_some());
            assert_eq!(
                resolve(&catalog, &catalog, "sql", name, reference, result).unwrap(),
                None
            );
        }
    }
    assert!(catalog.probes.borrow().is_empty());
    assert!(catalog.usage.borrow().is_empty());
    assert!(catalog.notices.borrow().is_empty());
}

#[test]
fn quoted_percent_type_names_still_validate_shell_usage_and_language() {
    let catalog = Catalog::default();
    let name = "\"source.value%TYPE\"";
    assert_eq!(
        resolve(&catalog, &catalog, "plpgsql", name, None, false).unwrap(),
        Some(16384)
    );
    assert_eq!(catalog.usage.borrow().as_slice(), &[16384]);
    assert_eq!(catalog.notices.borrow().len(), 1);
    let error = resolve(&catalog, &catalog, "sql", name, None, true).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P13"));
    assert_eq!(
        error.to_string(),
        "SQL function cannot return shell type source.value%TYPE"
    );
    assert_eq!(catalog.probes.borrow().as_slice(), &[name, name]);
    assert_eq!(catalog.usage.borrow().len(), 1);
    assert_eq!(catalog.notices.borrow().len(), 1);
}
