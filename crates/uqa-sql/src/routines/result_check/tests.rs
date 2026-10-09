//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{check_sql_function_result, sql_function_result_layout, SQLFunctionResultKind};
use crate::{
    ast::{ColumnDef, ColumnType, CompositeTypeReference, CreateFunction, Statement},
    binding::statements::AnalyzedResult,
    expr::composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor},
    routines::declaration::RoutineTypeCatalog,
    SQLError,
};

#[test]
fn caller_record_types_distinguish_sql_target_columns_from_whole_records() {
    let source = [Some(ColumnType::Integer), Some(ColumnType::Text)];
    let target = [
        super::SQLFunctionResultColumn {
            name: "widened".into(),
            ty: ColumnType::BigInteger,
        },
        super::SQLFunctionResultColumn {
            name: "label".into(),
            ty: ColumnType::Varchar(Some(8)),
        },
    ];
    super::validate_anonymous_record_result(
        &Types,
        &source,
        &target,
        Some(SQLFunctionResultKind::Tuple),
    )
    .unwrap();
    let whole = super::validate_anonymous_record_result(
        &Types,
        &source,
        &target,
        Some(SQLFunctionResultKind::Value),
    )
    .unwrap_err();
    assert_eq!(whole.sqlstate(), Some("42804"));
    assert_eq!(
        whole.detail(),
        Some("Returned type integer at ordinal position 1, but query expects bigint.")
    );
    let procedural =
        super::validate_anonymous_record_result(&Types, &source, &target, None).unwrap_err();
    assert_eq!(procedural.sqlstate(), Some("42804"));
    assert_eq!(procedural.detail(), Some("Returned type integer does not match expected type bigint in column \"widened\" (position 1)."));
}

struct Types;

impl RoutineTypeCatalog for Types {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }

    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        self.resolve_catalog_column_type_name(name).ok()
    }

    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        match name {
            "rb.pair" => Ok(pair(16384)),
            "rb.other" => Ok(pair(16385)),
            "rb.pair_domain" => Ok(ColumnType::Domain {
                schema: "rb".into(),
                name: "pair_domain".into(),
                array_oid: Some(16396),
                oid: 16386,
                base: Box::new(pair(16384)),
            }),
            _ => ColumnType::from_sql_name(name),
        }
    }

    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }

    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }

    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.regtype_name())
    }

    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
}

fn pair(oid: u32) -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "rb".into(),
        name: if oid == 16384 { "pair" } else { "other" }.into(),
        oid,
        array_oid: oid + 10,
        relation_oid: oid + 20,
    })
}

impl CompositeTypeCatalog for Types {
    fn composite_type(
        &self,
        type_oid: u32,
    ) -> Result<Option<std::sync::Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(Some(std::sync::Arc::new(CompositeTypeDescriptor {
            dropped: Vec::new(),
            type_oid,
            relation_oid: type_oid + 20,
            attributes: vec![
                CompositeAttribute {
                    name: "a".into(),
                    ty: ColumnType::Integer,
                    number: 1,
                },
                CompositeAttribute {
                    name: "b".into(),
                    ty: ColumnType::Text,
                    number: 3,
                },
            ],
        })))
    }
}

fn definition(header: &str) -> CreateFunction {
    let Statement::CreateFunction(definition) =
        crate::compile(&format!("CREATE {header} LANGUAGE sql AS 'SELECT 1'"))
            .unwrap()
            .remove(0)
    else {
        panic!("expected a routine definition");
    };
    *definition
}

/// The message and detail of the check, or `None` when it passes.
fn checked(header: &str, last: Option<AnalyzedResult>) -> Option<(String, String)> {
    match check_sql_function_result(&Types, &definition(header), last.as_ref()) {
        Ok(()) => None,
        Err(SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            ..
        }) => {
            assert_eq!(sqlstate, "42P13");
            Some((message, detail.unwrap()))
        }
        Err(other) => panic!("unexpected error {other:?}"),
    }
}

fn rows(types: &[Option<ColumnType>]) -> Option<AnalyzedResult> {
    Some(AnalyzedResult::Rows(types.to_vec()))
}

const INTEGER: &str = "return type mismatch in function declared to return integer";
const RECORD: &str = "return type mismatch in function declared to return record";

#[test]
fn a_void_function_may_end_with_any_statement() {
    assert_eq!(checked("FUNCTION f() RETURNS void", None), None);
    assert_eq!(
        checked("FUNCTION f() RETURNS void", Some(AnalyzedResult::Command)),
        None
    );
    assert_eq!(checked("PROCEDURE p()", None), None);
}

#[test]
fn a_scalar_result_needs_one_assignable_column() {
    let final_statement = |detail: &str| Some((INTEGER.to_string(), detail.to_string()));
    let must_return =
        "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING.";
    assert_eq!(
        checked("FUNCTION f() RETURNS int", None),
        final_statement(must_return)
    );
    assert_eq!(
        checked("FUNCTION f() RETURNS int", Some(AnalyzedResult::Command)),
        final_statement(must_return)
    );
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS int",
            rows(&[Some(ColumnType::Integer), Some(ColumnType::Integer)])
        ),
        final_statement("Final statement must return exactly one column.")
    );
    assert_eq!(
        checked("FUNCTION f() RETURNS int", rows(&[Some(ColumnType::Text)])),
        final_statement("Actual return type is text.")
    );
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS boolean",
            rows(&[Some(ColumnType::Integer)])
        ),
        Some((
            "return type mismatch in function declared to return boolean".into(),
            "Actual return type is integer.".into()
        ))
    );
    for source in [
        ColumnType::BigInteger,
        ColumnType::Numeric {
            precision: None,
            scale: None,
        },
        ColumnType::Integer,
    ] {
        assert_eq!(
            checked("FUNCTION f() RETURNS int", rows(&[Some(source)])),
            None
        );
    }
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS text",
            rows(&[Some(ColumnType::Integer)])
        ),
        None
    );
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS varchar(3)",
            rows(&[Some(ColumnType::Text)])
        ),
        None
    );
    // A single output column makes the function return that column's type.
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS TABLE(a int)",
            rows(&[Some(ColumnType::Text)])
        ),
        final_statement("Actual return type is text.")
    );
}

#[test]
fn a_row_result_checks_each_column_in_order() {
    let table = "FUNCTION f() RETURNS TABLE(a int, b int)";
    let detail = |detail: &str| Some((RECORD.to_string(), detail.to_string()));
    assert_eq!(
        checked(table, rows(&[Some(ColumnType::Integer)])),
        detail("Final statement returns too few columns.")
    );
    assert_eq!(
        checked(
            table,
            rows(&[
                Some(ColumnType::Integer),
                Some(ColumnType::Integer),
                Some(ColumnType::Integer)
            ])
        ),
        detail("Final statement returns too many columns.")
    );
    assert_eq!(
        checked(
            table,
            rows(&[
                Some(ColumnType::Text),
                Some(ColumnType::Integer),
                Some(ColumnType::Integer)
            ])
        ),
        detail("Final statement returns text instead of integer at column 1.")
    );
    assert_eq!(
        checked(
            table,
            rows(&[Some(ColumnType::Integer), Some(ColumnType::Text)])
        ),
        detail("Final statement returns text instead of integer at column 2.")
    );
    assert_eq!(
        checked(
            "FUNCTION f(OUT a int, OUT b text)",
            rows(&[Some(ColumnType::Integer)])
        ),
        detail("Final statement returns too few columns.")
    );
    assert_eq!(
        checked(table, rows(&[Some(ColumnType::Integer), None])),
        None
    );
    // A function returns a lone row column as its whole result; a procedure assigns its output parameters column by column.
    assert_eq!(checked(table, rows(&[Some(ColumnType::Record)])), None);
    assert_eq!(
        checked("PROCEDURE p(OUT a int)", rows(&[Some(ColumnType::Record)])),
        detail("Final statement returns record instead of integer at column 1.")
    );
    assert_eq!(
        checked("PROCEDURE p(OUT a int)", rows(&[Some(ColumnType::Integer)])),
        None
    );
}

#[test]
fn a_record_without_output_parameters_accepts_any_row() {
    for header in [
        "FUNCTION f() RETURNS record",
        "FUNCTION f() RETURNS SETOF record",
    ] {
        assert_eq!(
            checked(
                header,
                rows(&[Some(ColumnType::Integer), Some(ColumnType::Text)])
            ),
            None
        );
        assert_eq!(
            checked(header, Some(AnalyzedResult::Command)),
            Some((
                RECORD.to_string(),
                "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING."
                    .to_string()
            ))
        );
    }
}

#[test]
fn named_composite_results_distinguish_one_value_from_live_attribute_columns() {
    for header in [
        "FUNCTION f() RETURNS rb.pair",
        "FUNCTION f() RETURNS SETOF rb.pair",
        "FUNCTION f(OUT result rb.pair)",
    ] {
        let definition = definition(header);
        let expanded =
            AnalyzedResult::Rows(vec![Some(ColumnType::Integer), Some(ColumnType::Text)]);
        let layout = sql_function_result_layout(&Types, &definition, Some(&expanded)).unwrap();
        assert_eq!(layout.kind, SQLFunctionResultKind::Tuple);
        assert_eq!(
            layout
                .columns
                .unwrap()
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        let whole = AnalyzedResult::Rows(vec![Some(pair(16384))]);
        assert_eq!(
            sql_function_result_layout(&Types, &definition, Some(&whole))
                .unwrap()
                .kind,
            SQLFunctionResultKind::Value
        );
    }
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS rb.pair",
            rows(&[Some(ColumnType::Integer)])
        ),
        Some((
            "return type mismatch in function declared to return rb.pair".into(),
            "Final statement returns too few columns.".into()
        ))
    );
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS rb.pair_domain",
            rows(&[Some(ColumnType::Integer), Some(ColumnType::Text)])
        ),
        Some((
            "return type mismatch in function declared to return rb.pair_domain".into(),
            "Final statement must return exactly one column.".into()
        ))
    );
}

#[test]
fn anonymous_row_assignment_checks_shape_and_types_when_the_target_is_named() {
    for (fields, detail) in [
        (
            vec![Some(ColumnType::Integer)],
            "Input has too few columns.",
        ),
        (
            vec![Some(ColumnType::Text), Some(ColumnType::Integer)],
            "Cannot cast type text to integer in column 1.",
        ),
    ] {
        let schema =
            crate::RowSchema::with_types(vec!["row".into()], vec![Some(ColumnType::Record)])
                .with_record_fields([(0, fields.into())]);
        let error = check_sql_function_result(
            &Types,
            &definition("FUNCTION f() RETURNS rb.pair"),
            Some(&AnalyzedResult::Schema(schema)),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42846"));
        assert_eq!(error.to_string(), "cannot cast type record to rb.pair");
        assert_eq!(error.detail(), Some(detail));
    }
}

#[test]
fn a_whole_returned_record_preserves_integer_width_and_unknown_field_identity() {
    let target = [ColumnType::Integer, ColumnType::Text];
    for (source, detail) in [
        (
            vec![Some(ColumnType::BigInteger), Some(ColumnType::Text)],
            "Returned type bigint at ordinal position 1, but query expects integer.",
        ),
        (
            vec![None, None],
            "Returned type unknown at ordinal position 1, but query expects integer.",
        ),
    ] {
        let error = super::validate_sql_function_record(&Types, &source, &target).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        assert_eq!(error.detail(), Some(detail));
    }
}
