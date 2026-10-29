//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;

fn domain() -> ColumnType {
    ColumnType::Domain {
        collation_oid: None,
        schema: "public".into(),
        name: "positive".into(),
        oid: 16_384,
        array_oid: Some(16_385),
        base: Box::new(ColumnType::Integer),
    }
}

#[derive(Default)]
struct Inputs {
    elements: Cell<usize>,
}

impl EngineHook for Inputs {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!("the input adapter does not evaluate ordinary expressions")
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!("the input adapter does not evaluate ordinary expressions")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!("the input adapter does not evaluate ordinary expressions")
    }
    fn resolve_type_name(&self, name: &str) -> std::result::Result<Option<ColumnType>, String> {
        Ok((name == domain().catalog_name()).then(domain))
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &ColumnType,
    ) -> Result<Option<Value>> {
        assert_eq!(*target, domain());
        assert_eq!(source, None, "unknown elements use their input functions");
        self.elements.set(self.elements.get() + 1);
        match value {
            Value::Null => Err(SQLError::Routine {
                sqlstate: "23502".into(),
                message: "domain positive does not allow null values".into(),
            }),
            Value::Str(text) if text == "0" => Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: "value for domain positive violates check constraint \"positive_value\""
                    .into(),
            }),
            value => crate::expr::cast_value_from(value, "integer", None).map(Some),
        }
    }
}

#[test]
fn domain_array_input_preserves_multidimensional_bounds_and_checks_each_element_once() {
    let inputs = Inputs::default();
    let target = ColumnType::Array(Box::new(domain()));
    let Value::Array(array) =
        read_catalog_array_input("[0:1][3:4]={{1,2},{3,4}}", &target, &inputs).unwrap()
    else {
        panic!("array input")
    };
    assert_eq!(array.lower_bounds(), [0, 3]);
    assert_eq!(
        array.elements(),
        [
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(3), Value::Int(4)]),
        ]
    );
    assert_eq!(inputs.elements.get(), 4);
    assert!(
        matches!(read_catalog_array_input("{}", &target, &inputs).unwrap(), Value::Array(array) if array.elements().is_empty())
    );
    assert_eq!(inputs.elements.get(), 4);
}

#[test]
fn controlled_domain_array_input_retains_bounds_and_one_constraint_read_per_element() {
    let inputs = Inputs::default();
    let target = ColumnType::Array(Box::new(domain()));
    let budget = MemoryBudget::new(128 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let output =
        read_catalog_input_with_control("[0:1][3:4]={{1,2},{3,4}}", &target, &inputs, &control)
            .unwrap();
    let Value::Array(array) = &*output else {
        panic!("array input")
    };
    assert_eq!(array.lower_bounds(), [0, 3]);
    assert_eq!(inputs.elements.get(), 4);
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
    invoking.cancel();
    let error = read_catalog_input_with_control("{1}", &target, &inputs, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(inputs.elements.get(), 4);
}

#[test]
fn catalog_input_admits_large_text_before_constructing_a_native_datum() {
    let text = format!("<root>{}</root>", "x".repeat(4 << 20));
    let target = ColumnType::Xml;
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let input = Inputs::default();
    let allocation = allocation_counter::measure(|| {
        let error = read_catalog_input_with_control(&text, &target, &input, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_max < 4096, "{allocation:?}");
    assert_eq!(allocation.bytes_current, 0);
    assert_eq!(budget.used(), 0);
    original.cancel();
    assert!(control.check().is_err());
    let allocation = allocation_counter::measure(|| {
        let error = read_catalog_input_with_control(&text, &target, &input, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn domain_array_input_stops_at_the_first_element_failure() {
    let target = ColumnType::Array(Box::new(domain()));
    for (text, state) in [("{1,0,not_read}", "23514"), ("{1,NULL,not_read}", "23502")] {
        let inputs = Inputs::default();
        let failure = read_catalog_array_input(text, &target, &inputs).unwrap_err();
        assert_eq!(failure.sqlstate(), Some(state));
        assert_eq!(inputs.elements.get(), 2);
    }
}
