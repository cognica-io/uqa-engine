//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical replacement retains the original input and both cancellation scopes.

use super::*;

struct Replacing<'a> {
    cancel: Option<&'a CancellationToken>,
    fail: bool,
    calls: Cell<usize>,
}

impl RangeConstruction for Replacing<'_> {
    type Identity = u32;
    fn identity(&self) -> u32 {
        101_000
    }
    fn has_canonical_function(&self) -> bool {
        true
    }
    fn input_bound(
        &self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        Ok(control.copy_value(value)?)
    }
    fn compare_bounds(
        &self,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        Ok(left.cmp_with_control(right, control)?)
    }
    fn canonicalize(
        &self,
        range: &CanonicalRange<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<CanonicalRange<u32>>>> {
        self.calls.set(self.calls.get() + 1);
        let lower = copy_bound(range.lower(), control)?;
        let upper = copy_bound(range.upper(), control)?;
        let (lower, lower_memory) = lower.into_parts();
        let (upper, upper_memory) = upper.into_parts();
        let owned = RangeWorkspace {
            value: CanonicalRange {
                subtype: self.identity(),
                lower,
                upper,
                lower_inclusive: range.lower_inclusive,
                upper_inclusive: range.upper_inclusive,
                empty: range.empty,
            },
            memory: control.combine(lower_memory, upper_memory),
        };
        if let Some(cancel) = self.cancel {
            cancel.cancel();
        }
        if self.fail {
            return Err(SQLError::Routine {
                sqlstate: "P0001".into(),
                message: "selected canonical failure".into(),
            });
        }
        Ok(Some(control.finish(owned.value, owned.memory)?))
    }
}

#[test]
fn selected_range_canonical_replacement_preserves_quota_and_cancellation_owners() {
    let lower = "a".repeat(192);
    let upper = "z".repeat(192);
    let range_input = format!("[{lower},{upper})");
    let multi_input = format!("{{{range_input},[b,y],[~a,~z]}}");
    let mut completed = 0;
    let mut denied = 0;
    for allowance in [0, 8, 32, 256, 512, 1024, 2048, 8192, 32768] {
        let memory = MemoryBudget::new(allowance);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let prior = control.copy_value(&Value::Int(17)).unwrap();
        let operations = Replacing {
            cancel: None,
            fail: false,
            calls: Cell::new(0),
        };
        match parse_range_with_operations(&range_input, &operations, &control) {
            Ok(value) => {
                assert_eq!(value.lower(), Some(&Value::Str(lower.clone())));
                assert_eq!(memory.used(), value.reserved_bytes());
                assert_eq!(operations.calls.get(), 1);
                completed += 1;
            }
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                denied += 1;
            }
        }
        assert_eq!(memory.used(), 0);
        match parse_multirange_with_operations(&multi_input, &operations, &control) {
            Ok(value) => {
                assert_eq!(value.ranges().len(), 2);
                assert_eq!(memory.used(), value.reserved_bytes());
                completed += 1;
            }
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                denied += 1;
            }
        }
        assert_eq!(memory.used(), 0);
        assert_eq!(&*prior, &Value::Int(17));
    }
    assert!(completed > 0 && denied > 0);

    let memory = MemoryBudget::new(32768);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let prior = control.copy_text("retained caller value").unwrap();
    let before = memory.used();
    for cancel in [&original, &invoking] {
        let operations = Replacing {
            cancel: Some(cancel),
            fail: false,
            calls: Cell::new(0),
        };
        assert_eq!(
            parse_range_with_operations(&range_input, &operations, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(operations.calls.get(), 1);
        assert_eq!(memory.used(), before);
        cancel.reset();
        assert_eq!(
            parse_multirange_with_operations(&multi_input, &operations, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), before);
        cancel.reset();
        let operations = Replacing {
            cancel: Some(cancel),
            fail: true,
            calls: Cell::new(0),
        };
        assert_eq!(
            parse_range_with_operations(&range_input, &operations, &control)
                .unwrap_err()
                .sqlstate(),
            Some("P0001")
        );
        assert_eq!(memory.used(), before);
        cancel.reset();
    }
    assert_eq!(&**prior, "retained caller value");
    drop(prior);
    assert_eq!(memory.used(), 0);
}

#[test]
fn selected_range_empty_and_typed_members_do_not_repeat_input_or_canonical_calls() {
    let memory = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let operations = Operations::new(Kind::Counted);
    for text in ["empty", "[1,1)"] {
        assert!(parse_range_with_operations(text, &operations, &control)
            .unwrap()
            .is_empty());
    }
    assert_eq!(operations.canonicals.get(), 0);
    let mut ranges = ProductionVec::new(control);
    ranges
        .push_produced(parse_range_with_operations("[1,2)", &operations, &control).unwrap())
        .unwrap();
    ranges
        .push_produced(parse_range_with_operations("[2,3)", &operations, &control).unwrap())
        .unwrap();
    assert_eq!(operations.canonicals.get(), 2);
    let multi =
        multirange_from_ranges_with_operations(ranges.finish().unwrap(), &operations, &control)
            .unwrap();
    assert_eq!(multi.ranges().len(), 1);
    assert_eq!(operations.canonicals.get(), 3);
    // Reading the completed carrier exposes its fields without another invocation.
    assert_eq!(multi.ranges()[0].lower(), Some(&Value::Int(1)));
    assert_eq!(multi.ranges()[0].upper(), Some(&Value::Int(3)));
    assert_eq!(operations.canonicals.get(), 3);
    drop(multi);
    assert_eq!(memory.used(), 0);
}

#[test]
fn repeated_range_union_releases_superseded_canonical_replacements() {
    let memory = MemoryBudget::new(2 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let operations = Replacing {
        cancel: None,
        fail: false,
        calls: Cell::new(0),
    };
    let input = format!("[{},{}]", "a".repeat(128), "z".repeat(128));
    let mut ranges = ProductionVec::new(control);
    for _ in 0..64 {
        ranges
            .push_produced(parse_range_with_operations(&input, &operations, &control).unwrap())
            .unwrap();
    }
    let ranges = ranges.finish().unwrap();
    let input_bytes = ranges.reserved_bytes();
    let output = multirange_from_ranges_with_operations(ranges, &operations, &control).unwrap();
    assert_eq!(output.ranges().len(), 1);
    assert_eq!(operations.calls.get(), 64 + 63);
    // Two admitted member arrays plus the live replacement fit; historical replacement payloads must not accumulate.
    let workspace_bytes = 2 * 64 * std::mem::size_of::<CanonicalRange<u32>>();
    assert!(output.reserved_bytes() <= input_bytes + workspace_bytes + 2 * 128);
    assert_eq!(memory.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(memory.used(), 0);
}

struct Comparing {
    calls: Cell<usize>,
    failure: Option<usize>,
}

impl RangeConstruction for Comparing {
    type Identity = u32;
    fn identity(&self) -> u32 {
        Operations::new(Kind::Plain).identity()
    }
    fn has_canonical_function(&self) -> bool {
        false
    }
    fn input_bound(
        &self,
        _value: &Value,
        _control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        panic!("normalization must not repeat member input");
    }
    fn compare_bounds(
        &self,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        let calls = self.calls.get() + 1;
        self.calls.set(calls);
        if self.failure == Some(calls) {
            return Err(SQLError::Routine {
                sqlstate: "P0001".into(),
                message: "selected comparison failure".into(),
            });
        }
        Ok(left.cmp_with_control(right, control)?)
    }
}

#[test]
fn selected_multirange_ordering_bounds_work_and_releases_comparison_failures() {
    let memory = MemoryBudget::new(2 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let prior = control.copy_text("retained before sorting").unwrap();
    let before = memory.used();
    for failure in [Some(1), Some(70), Some(500), None] {
        let mut ranges = ProductionVec::new(control);
        let operations = Operations::new(Kind::Plain);
        for index in 0..512 {
            let lower = (index * 17) % 512;
            let lower_value = control
                .finish(Some(Value::Int(lower)), control.empty_reservation())
                .unwrap();
            let upper_value = control
                .finish(Some(Value::Int(lower + 1)), control.empty_reservation())
                .unwrap();
            ranges
                .push_produced(
                    range_from_bounds_with_operations(
                        lower_value,
                        upper_value,
                        true,
                        false,
                        &operations,
                        &control,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let compare = Comparing {
            calls: Cell::new(0),
            failure,
        };
        let result =
            multirange_from_ranges_with_operations(ranges.finish().unwrap(), &compare, &control);
        if let Some(failure) = failure {
            assert_eq!(result.unwrap_err().sqlstate(), Some("P0001"));
            assert_eq!(compare.calls.get(), failure);
        } else {
            let output = result.unwrap();
            assert_eq!(output.ranges().len(), 1);
            assert_eq!(output.ranges()[0].lower(), Some(&Value::Int(0)));
            assert_eq!(output.ranges()[0].upper(), Some(&Value::Int(512)));
            assert!(compare.calls.get() <= 512 * 64);
        }
        assert_eq!(memory.used(), before);
    }
    assert_eq!(&**prior, "retained before sorting");
    drop(prior);
    assert_eq!(memory.used(), 0);
}
