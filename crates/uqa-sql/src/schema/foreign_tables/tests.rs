//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::Statement;
use std::cell::RefCell;
use uqa_core::Value;

fn column(declaration: &str) -> ColumnDef {
    let Statement::CreateTable(table) =
        crate::compile(&format!("CREATE TABLE fixture ({declaration})"))
            .unwrap()
            .remove(0)
    else {
        panic!("expected table declaration")
    };
    table.columns.into_iter().next().unwrap()
}

#[test]
fn duplicate_column_precedes_its_constraint_validation() {
    let first = column("id integer");
    let mut duplicate = first.clone();
    duplicate.primary_key = true;
    let error = validate_foreign_table_schema_envelope(&[first, duplicate]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42701"));
    assert_eq!(error.to_string(), "column \"id\" specified more than once");
}

#[test]
fn primary_key_diagnostic_precedes_other_column_constraints() {
    let mut value = column("id integer REFERENCES parent(id)");
    value.primary_key = true;
    value.unique = true;
    let error = validate_foreign_table_schema_envelope(&[value.clone()]).unwrap_err();
    assert!(error.to_string().contains("primary key constraints"));
    value.primary_key = false;
    let error = validate_foreign_table_schema_envelope(&[value]).unwrap_err();
    assert!(error.to_string().contains("unique constraints"));
}

#[test]
fn foreign_key_column_is_rejected_without_binding_its_target() {
    let value = column("id integer REFERENCES missing_parent(id)");
    let error = validate_foreign_table_schema_envelope(&[value]).unwrap_err();
    assert!(error
        .to_string()
        .contains("foreign key constraints are not supported on foreign tables"));
}

#[test]
fn supported_envelope_preserves_unbound_expressions_and_identity_fields() {
    let columns = vec![
        column("id integer DEFAULT missing_function() CHECK (id > 0)"),
        column("computed integer GENERATED ALWAYS AS (id + 1) STORED"),
    ];
    let before = serde_json::to_string(&columns).unwrap();
    validate_foreign_table_schema_envelope(&columns).unwrap();
    assert_eq!(serde_json::to_string(&columns).unwrap(), before);
}

#[derive(Default)]
struct References {
    calls: RefCell<Vec<String>>,
    fail_sequence: bool,
    missing_relation: bool,
}
impl References {
    fn record(&self, kind: &str, name: &str) {
        self.calls.borrow_mut().push(format!("{kind}:{name}"));
    }
}
impl SchemaReferenceCatalog for References {
    fn loaded_relation_name(&self, reference: &str) -> Result<Option<String>, String> {
        self.record("loaded-relation", reference);
        Ok((!self.missing_relation).then(|| "stored.seq".into()))
    }
    fn bound_relation_oid(&self, canonical: &str) -> Result<Option<i64>, String> {
        self.record("bound-oid", canonical);
        Ok(Some(41))
    }
    fn visible_relation_oid(&self, reference: &str) -> Result<Option<i64>, String> {
        self.record("visible-oid", reference);
        Ok(Some(42))
    }
    fn sequence_for_binding(&self, reference: &str) -> Result<String, String> {
        self.record("current-sequence", reference);
        Ok("visible.seq".into())
    }
}
impl StoredSequenceNames for References {
    fn stored_sequence_name(&self, reference: &str) -> Result<String, String> {
        self.record("stored-sequence", reference);
        if self.fail_sequence {
            Err("stored sequence is ambiguous".into())
        } else {
            Ok("stored.seq".into())
        }
    }
}
fn sequence_expression(argument: Expr) -> Expr {
    Expr::Func {
        name: "nextval".into(),
        binding: None,
        args: vec![argument],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

#[test]
fn loaded_sequence_binding_does_not_consult_the_session_search_path() {
    let references = References::default();
    let mut expression = sequence_expression(Expr::Literal(Value::Str("seq".into())));
    prepare_foreign_table_sequence_references(&references, &references, &mut expression, true)
        .unwrap();
    assert_eq!(*references.calls.borrow(), ["stored-sequence:seq"]);
    let Expr::Func { args, .. } = expression else {
        panic!("expected function")
    };
    assert!(matches!(&args[0],Expr::Literal(Value::Str(name)) if name=="stored.seq"));
}

#[test]
fn current_sequence_binding_uses_visible_names_before_storing_the_reference() {
    let references = References::default();
    let mut expression = sequence_expression(Expr::Literal(Value::Str("seq".into())));
    prepare_foreign_table_sequence_references(&references, &references, &mut expression, false)
        .unwrap();
    assert_eq!(*references.calls.borrow(), ["current-sequence:seq"]);
    let Expr::Func { args, .. } = expression else {
        panic!("expected function")
    };
    assert!(matches!(&args[0],Expr::Literal(Value::Str(name)) if name=="visible.seq"));
}

#[test]
fn bound_regclass_is_resolved_before_any_sequence_name_rewrite() {
    let references = References::default();
    let mut expression = sequence_expression(Expr::Cast {
        expr: Box::new(Expr::Literal(Value::Str("seq".into()))),
        ty: "regclass".into(),
    });
    prepare_foreign_table_sequence_references(&references, &references, &mut expression, true)
        .unwrap();
    assert_eq!(
        *references.calls.borrow(),
        ["loaded-relation:seq", "bound-oid:stored.seq"]
    );
    let Expr::Func { args, .. } = expression else {
        panic!("expected function")
    };
    assert!(
        matches!(&args[0],Expr::Cast {expr,..} if matches!(expr.as_ref(),Expr::TypedLiteral {value:Value::Int(41),..}))
    );
}

#[test]
fn reference_failures_preserve_diagnostics_and_stop_before_later_binding() {
    let references = References {
        missing_relation: true,
        ..References::default()
    };
    let mut expression = sequence_expression(Expr::Cast {
        expr: Box::new(Expr::Literal(Value::Str("seq".into()))),
        ty: "regclass".into(),
    });
    let error =
        prepare_foreign_table_sequence_references(&references, &references, &mut expression, true)
            .unwrap_err();
    assert!(error
        .to_string()
        .contains("relation \"seq\" does not exist"));
    assert_eq!(*references.calls.borrow(), ["loaded-relation:seq"]);
    let references = References {
        fail_sequence: true,
        ..References::default()
    };
    let mut expression = sequence_expression(Expr::Literal(Value::Str("seq".into())));
    let error =
        prepare_foreign_table_sequence_references(&references, &references, &mut expression, true)
            .unwrap_err();
    assert!(error.to_string().contains("stored sequence is ambiguous"));
    assert_eq!(*references.calls.borrow(), ["stored-sequence:seq"]);
}
