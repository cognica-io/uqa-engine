//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` observations of input, canonical calls, normalization and diagnostics.

use super::*;
use crate::ast::RangeSubtype;
use crate::expr::range::{production::write_range_with, semantics::RangeSemantics};
use serde::Deserialize;
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryBudget, ProductionString},
    CancellationToken,
};

mod resources;

#[derive(Deserialize)]
struct Fixture {
    observations: Vec<Observation>,
    cases: Vec<Reference>,
}
#[derive(Deserialize)]
struct Observation {
    id: String,
    r#type: String,
    input: String,
    #[serde(default)]
    typed: bool,
}
#[derive(Deserialize)]
struct Reference {
    id: String,
    error: Option<ReferenceError>,
    results: Vec<ReferenceRows>,
}
#[derive(Deserialize)]
struct ReferenceRows {
    rows: Vec<Vec<Option<String>>>,
}
#[derive(Deserialize)]
struct ReferenceError {
    sqlstate: String,
    message: String,
    detail: Option<String>,
    hint: Option<String>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("tests/postgresql.json")).unwrap()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Domain,
    Counted,
    Discrete,
    Reverse,
    Text,
    Numeric,
}

struct Operations {
    kind: Kind,
    inputs: Cell<u64>,
    canonicals: Cell<u64>,
}

impl Operations {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            inputs: Cell::new(0),
            canonicals: Cell::new(0),
        }
    }
    fn native(&self) -> &'static RangeSemantics {
        RangeSemantics::native(if self.kind == Kind::Numeric {
            RangeSubtype::Numeric
        } else {
            RangeSubtype::Integer
        })
    }
    fn output(&self, value: &Value, control: &ProductionControl<'_>) -> Result<Produced<String>> {
        if let Value::Str(text) = value {
            return Ok(control.copy_text(text)?);
        }
        self.native().output(value, control)
    }
}

impl RangeConstruction for Operations {
    type Identity = u32;
    fn identity(&self) -> u32 {
        100_000 + self.kind as u32
    }
    fn has_canonical_function(&self) -> bool {
        matches!(self.kind, Kind::Counted | Kind::Discrete)
    }
    fn input_bound(
        &self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        if self.kind == Kind::Text {
            return Ok(control.copy_value(value)?);
        }
        let value = self.native().input(value, control)?;
        if self.kind == Kind::Domain {
            self.inputs.set(self.inputs.get() + 1);
            if !matches!(&*value, Value::Int(n) if *n > 0) {
                return Err(SQLError::Routine { sqlstate: "23514".into(), message: "value for domain positive_bound violates check constraint \"positive_bound_check\"".into() });
            }
        }
        Ok(value)
    }
    fn compare_bounds(
        &self,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        let ordering = if self.kind == Kind::Text {
            left.cmp_with_control(right, control)?
        } else {
            self.native().compare(left, right, control)?
        };
        Ok(if self.kind == Kind::Reverse {
            ordering.reverse()
        } else {
            ordering
        })
    }
    fn canonicalize(
        &self,
        range: &CanonicalRange<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<CanonicalRange<u32>>>> {
        self.canonicals.set(self.canonicals.get() + 1);
        if self.kind != Kind::Discrete {
            return Ok(None);
        }
        let lower = copy_bound(range.lower(), control)?;
        let upper = copy_bound(range.upper(), control)?;
        let output = range_from_bounds_with_operations(
            lower,
            upper,
            range.lower_inclusive,
            range.upper_inclusive,
            self.native(),
            control,
        )?;
        let (output, memory) = output.into_parts();
        Ok(Some(control.finish(
            CanonicalRange {
                subtype: self.identity(),
                lower: output.lower,
                upper: output.upper,
                lower_inclusive: output.lower_inclusive,
                upper_inclusive: output.upper_inclusive,
                empty: output.empty,
            },
            memory,
        )?))
    }
}

fn reference<'a>(fixture: &'a Fixture, id: &str) -> &'a Reference {
    fixture.cases.iter().find(|case| case.id == id).unwrap()
}

fn assert_error(error: &SQLError, expected: &ReferenceError) {
    assert_eq!(error.sqlstate(), Some(expected.sqlstate.as_str()));
    assert_eq!(error.to_string(), expected.message);
    assert_eq!(error.detail(), expected.detail.as_deref());
    assert_eq!(error.hint(), expected.hint.as_deref());
}

fn render(
    input: &str,
    multi: bool,
    typed: bool,
    operations: &Operations,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    if multi {
        let ranges = if typed {
            let mut items = crate::expr::range::input::MultirangeInput::new(input, control)?;
            let mut ranges = ProductionVec::new(*control);
            while let Some(item) = items.next_range(control)? {
                ranges.push_produced(parse_range_with_operations(item, operations, control)?)?;
            }
            multirange_from_ranges_with_operations(ranges.finish()?, operations, control)?
        } else {
            parse_multirange_with_operations(input, operations, control)?
        };
        output.push('{')?;
        for (index, range) in ranges.ranges().iter().enumerate() {
            if index != 0 {
                output.push(',')?;
            }
            write_range_with(range, &mut output, control, |value, control| {
                operations.output(value, control)
            })?;
        }
        output.push('}')?;
    } else {
        let range = parse_range_with_operations(input, operations, control)?;
        write_range_with(&range, &mut output, control, |value, control| {
            operations.output(value, control)
        })?;
    }
    Ok(output.finish()?)
}

#[test]
fn selected_range_construction_matches_postgresql_input_effects_canonical_calls_and_output() {
    let fixture = fixture();
    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for observation in &fixture.observations {
        let kind = match observation.r#type.split('_').next().unwrap() {
            "plain" => Kind::Plain,
            "domain" => Kind::Domain,
            "counted" => Kind::Counted,
            "discrete" => Kind::Discrete,
            "reverse" => Kind::Reverse,
            "text" => Kind::Text,
            "numrange" | "nummultirange" => Kind::Numeric,
            name => panic!("unexpected family {name}"),
        };
        let operations = Operations::new(kind);
        let expected = reference(&fixture, &observation.id);
        match render(
            &observation.input,
            observation.r#type.contains("multirange"),
            observation.typed,
            &operations,
            &control,
        ) {
            Ok(output) => {
                assert!(expected.error.is_none(), "{}", observation.id);
                assert_eq!(
                    Some(output.as_str()),
                    expected.results[0].rows[0][0].as_deref(),
                    "{}",
                    observation.id
                );
                assert_eq!(memory.used(), output.reserved_bytes());
            }
            Err(error) => assert_error(&error, expected.error.as_ref().unwrap()),
        }
        let calls = &reference(&fixture, &(observation.id.clone() + "_calls")).results[0].rows[0];
        assert_eq!(
            operations.inputs.get().to_string(),
            *calls[0].as_ref().unwrap(),
            "{}",
            observation.id
        );
        if kind == Kind::Counted {
            assert_eq!(
                operations.canonicals.get().to_string(),
                *calls[1].as_ref().unwrap(),
                "{}",
                observation.id
            );
        }
        assert_eq!(memory.used(), 0, "{}", observation.id);
    }
}

#[test]
fn typed_range_constructors_skip_input_and_match_postgresql_null_bounds_and_flag_diagnostics() {
    let fixture = fixture();
    let memory = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let operations = Operations::new(Kind::Domain);
    for (index, lower, upper, flags) in [
        (0, None, None, Some("[]")),
        (1, Some(Value::Int(1)), Some(Value::Int(4)), Some("(]")),
    ] {
        let lower = copy_bound(lower.as_ref(), &control).unwrap();
        let upper = copy_bound(upper.as_ref(), &control).unwrap();
        let flags = range_bound_flags(flags).unwrap();
        let range = range_from_bounds_with_operations(
            lower,
            upper,
            flags.0,
            flags.1,
            &operations,
            &control,
        )
        .unwrap();
        let mut output = ProductionString::new(control);
        write_range_with(&range, &mut output, &control, |value, control| {
            operations.output(value, control)
        })
        .unwrap();
        let output = output.finish().unwrap();
        assert_eq!(
            Some(output.as_str()),
            reference(&fixture, &format!("constructor_{index}")).results[0].rows[0][0].as_deref()
        );
        assert_eq!(operations.inputs.get(), 0);
    }
    for (index, flags) in [(2, None), (3, Some("bad"))] {
        let expected = reference(&fixture, &format!("constructor_{index}"))
            .error
            .as_ref()
            .unwrap();
        assert_error(&range_bound_flags(flags).unwrap_err(), expected);
        let flag_value = flags
            .map(|flags| Value::Str(flags.into()))
            .unwrap_or(Value::Null);
        let error = crate::expr::scalar_range::eval_range_functions_with_control(
            "int4range",
            &[Value::Int(1), Value::Int(4), flag_value],
            &control,
        )
        .unwrap()
        .unwrap_err();
        assert_error(&error, expected);
    }
    assert_eq!(memory.used(), 0);
}
