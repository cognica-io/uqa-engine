//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn table_result_descriptions_bound_repeated_declaration_work() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (mut function, _) = bound_call();
    Arc::get_mut(&mut function).unwrap().def.returns = FunctionReturns::SetOf {
        type_name: "public.accepted_value".into(),
    };
    for index in 0..512 {
        let layout =
            crate::routines::result_check::declared_sql_function_result(&types, &function.def)
                .unwrap();
        assert_eq!(layout.kind, SQLFunctionResultKind::Value);
        assert_eq!(layout.declared_type, values.domain.column_type());
        assert!(layout.columns.is_none());
        let input = if index % 17 == 0 {
            Value::Null
        } else {
            Value::Int(index)
        };
        assert_eq!(
            crate::assignment::routines::coerce_routine_value_from(
                &values,
                &input,
                "public.accepted_value",
                Some(&ColumnType::Integer),
            )
            .unwrap(),
            input
        );
    }
    assert_eq!(
        values.checks.get(),
        512,
        "each current domain check remains live"
    );
    assert_eq!(
        types.calls.get(),
        1,
        "one declaration for the fixed table call"
    );
}
