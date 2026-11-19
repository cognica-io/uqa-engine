//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, FunctionBody, Statement},
    routines::{RoutineBody, SQLUserFunction, StoredRoutineDefinition},
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

struct Catalog;

impl RoutineTypeCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }
    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.sql_name())
    }
}

impl RoutineRegclassCatalog for Catalog {
    fn resolve_routine_regclass(&self, name: &str) -> Result<Option<i64>, SQLError> {
        Ok((name == "present").then_some(72))
    }
}

fn parse(sql: &str) -> CreateFunction {
    let Statement::CreateFunction(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("stored function declaration")
    };
    *definition
}

enum Stop {
    Complete,
    Cancel(usize),
    Deny,
}

type ResultImage = Result<(bool, serde_json::Value), (String, String)>;

fn run(definition: CreateFunction, steps: usize, stop: Stop) -> (ResultImage, usize) {
    let source = SQLUserFunction::new(definition, RoutineBody::Source).shared();
    let original_body = serde_json::to_value(&source.def.body).unwrap();
    let memory = MemoryBudget::new(32 << 20);
    let native = memory.clone();
    let token = CancellationToken::new();
    let original = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &token);
    let mut definition = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
    while !definition.copy_next_field(&control).unwrap() {}
    let barrier =
        matches!(stop, Stop::Deny).then(|| memory.reserve((32 << 20) - memory.used() - 1).unwrap());
    let workspace = MemoryBudget::new(2 << 20);
    let mut reader = ImageDecoder::new(&workspace, &token, |mut source| async move {
        let mut input = source.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let mut binding = RoutineRegclassBinding::default();
        loop {
            match binding
                .advance_from(&Catalog, &mut definition, &control, &mut input)
                .await
            {
                Ok(Some(changed)) => {
                    break Ok((
                        changed,
                        serde_json::to_value(definition.declaration()).unwrap(),
                    ))
                }
                Ok(None) => (),
                Err(error) => {
                    assert_eq!(
                        serde_json::to_value(&definition.declaration().body).unwrap(),
                        original_body
                    );
                    break Err(error);
                }
            }
        }
    })
    .unwrap();
    let mut polls = 0;
    let result = loop {
        polls += 1;
        if matches!(stop,Stop::Cancel(at) if at==polls) {
            token.cancel();
        }
        match reader.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
            Ok(None) => (),
            Ok(Some(result)) => break Ok(result),
            Err(error) => break Err((error.sqlstate().unwrap().to_owned(), error.to_string())),
        }
    };
    drop((reader, barrier));
    assert_eq!(workspace.used(), 0);
    assert_eq!(memory.used(), 0);
    (result, polls)
}

#[test]
fn stored_regclass_fields_preserve_typed_ids_defaults_and_search_failures() {
    let definition=parse("CREATE FUNCTION saved(p regclass DEFAULT 'present') RETURNS bigint LANGUAGE SQL BEGIN ATOMIC SELECT nextval('present'); SELECT 'present'::pg_catalog.regclass; END");
    let mut ordinary = definition.clone();
    assert!(bind_routine_regclass_constants(&Catalog, &Catalog, &mut ordinary).unwrap());
    let (changed, actual) = run(definition.clone(), 1, Stop::Complete).0.unwrap();
    assert!(changed);
    assert_eq!(actual, serde_json::to_value(&ordinary).unwrap());
    assert!(matches!(
        &ordinary.params[0].default,
        Some(Expr::TypedLiteral {
            value: Value::Int(72),
            ..
        })
    ));
    assert!(!run(ordinary, 32, Stop::Complete).0.unwrap().0);

    let mut invalid = definition;
    invalid.params[0].default = Some(Expr::Literal(Value::Str("missing_arg".into())));
    invalid.body =
        FunctionBody::Statements(crate::compile("SELECT nextval('missing_body')").unwrap());
    let expected =
        bind_routine_regclass_constants(&Catalog, &Catalog, &mut invalid.clone()).unwrap_err();
    assert_eq!(
        expected.to_string(),
        "relation \"missing_arg\" does not exist"
    );
    assert_eq!(
        run(invalid, 1, Stop::Complete).0.unwrap_err(),
        ("42P01".into(), expected.to_string())
    );
}

fn values(width: usize, missing: Option<usize>) -> CreateFunction {
    let mut definition = parse("CREATE FUNCTION saved() RETURNS bigint LANGUAGE SQL RETURN 1");
    let rows = (0..width)
        .map(|position| match position {
            value if Some(value) == missing => "(nextval('missing'))",
            0 => "(nextval('present'))",
            _ => "(1)",
        })
        .collect::<Vec<_>>()
        .join(",");
    definition.body = FunctionBody::Statements(crate::compile(&format!("VALUES {rows}")).unwrap());
    definition
}

#[test]
fn stored_regclass_statement_edits_yield_before_late_lookup_and_release_failed_scratch() {
    let (early, first) = run(values(4096, Some(1)), 1, Stop::Complete);
    let (late, last) = run(values(4096, Some(4095)), 1, Stop::Complete);
    assert_eq!(
        early.unwrap_err(),
        ("42P01".into(), "relation \"missing\" does not exist".into())
    );
    assert_eq!(
        late.unwrap_err(),
        ("42P01".into(), "relation \"missing\" does not exist".into())
    );
    assert!(last > first + 4090);
    let (batched, fewer) = run(values(4096, Some(4095)), 32, Stop::Complete);
    assert_eq!(batched.unwrap_err().0, "42P01");
    assert!(fewer < last / 8);
    assert_eq!(
        run(values(4096, Some(4095)), 1, Stop::Cancel(first + 128))
            .0
            .unwrap_err()
            .0,
        "57014"
    );
    assert_eq!(
        run(values(4096, None), 1, Stop::Deny).0.unwrap_err().0,
        "53200"
    );
    assert!(run(values(4096, None), 32, Stop::Complete).0.unwrap().0);
}
