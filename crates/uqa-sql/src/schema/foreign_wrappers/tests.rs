//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::Statement;

fn declaration(sql: &str) -> crate::ast::CreateForeignWrapper {
    let Statement::CreateForeignWrapper(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("wrapper declaration");
    };
    definition
}

#[test]
fn compilation_preserves_clause_order_and_defers_qualified_name_errors() {
    let definition = declaration("CREATE FOREIGN DATA WRAPPER w VALIDATOR a.b.c.d NO HANDLER NO VALIDATOR OPTIONS (z 'last',a 'first',z 'again')");
    assert_eq!(
        definition.functions,
        vec![
            ForeignWrapperFunctionOption::Validator(Some(vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into()
            ])),
            ForeignWrapperFunctionOption::Handler(None),
            ForeignWrapperFunctionOption::Validator(None),
        ]
    );
    assert_eq!(
        definition.options,
        [
            ("z".into(), "last".into()),
            ("a".into(), "first".into()),
            ("z".into(), "again".into())
        ]
    );
    let error = bind_functions(&definition.functions, |_, _, _| {
        panic!("invalid name precedes lookup")
    })
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("42601"));
    assert_eq!(
        error.to_string(),
        "improper qualified name (too many dotted names): a.b.c.d"
    );
}

#[test]
fn duplicate_function_clauses_are_checked_after_earlier_lookups() {
    let definition = declaration("CREATE FOREIGN DATA WRAPPER w HANDLER absent NO HANDLER");
    let mut visited = Vec::new();
    let error = bind_functions(&definition.functions, |name, args, _| {
        visited.push(name.to_owned());
        assert!(args.is_empty());
        Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: "missing handler".into(),
        })
    })
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(visited, ["absent"]);
    let definition = declaration("CREATE FOREIGN DATA WRAPPER w NO HANDLER HANDLER absent");
    assert_eq!(
        bind_functions(&definition.functions, |_, _, _| panic!(
            "duplicate before lookup"
        ))
        .err()
        .unwrap()
        .sqlstate(),
        Some("42601")
    );
}

#[test]
fn validator_requests_exact_signature_and_ignores_return_type() {
    let definition = declaration("CREATE FOREIGN DATA WRAPPER w VALIDATOR \"Mixed.Name\"");
    let functions = bind_functions(&definition.functions, |name, args, display| {
        assert_eq!(name, "\"Mixed.Name\"");
        assert_eq!(args, [1009, 26]);
        assert_eq!(display, ["text[]", "oid"]);
        Ok(ForeignWrapperRoutine {
            oid: 20001,
            return_oid: 23,
            binding: FunctionBinding {
                object_id: Some([1; 16]),
                name: name.into(),
                argument_types: display.to_vec(),
                builtin: false,
                dispatch: None,
                invocation: None,
                resolution_error: None,
            },
        })
    })
    .unwrap();
    assert_eq!(functions.validator.unwrap().return_oid, 23);
}

#[test]
fn option_validation_preserves_written_order() {
    let options = vec![("z".into(), "last".into()), ("a".into(), "first".into())];
    assert_eq!(creation_options(&options).unwrap(), options);
    assert_eq!(
        creation_options(&[("a".into(), "1".into()), ("a".into(), "2".into())])
            .unwrap_err()
            .sqlstate(),
        Some("42710")
    );
    assert_eq!(
        creation_options(&[("a=b".into(), "1".into())])
            .unwrap_err()
            .sqlstate(),
        Some("22023")
    );
}
