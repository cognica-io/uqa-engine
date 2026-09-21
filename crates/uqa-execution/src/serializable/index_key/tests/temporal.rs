//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal observations retain typed comparisons, parsed future boundaries and original controls.

use super::*;

#[test]
fn temporal_keys_cover_native_predicates_and_future_text_matches() {
    for (ty, texts) in [
        (ColumnType::Date, ["1969-12-31", "1970-01-01", "2024-02-29"]),
        (
            ColumnType::TimePrecision(6),
            ["00:00:00", "12:00:00.123456", "24:00:00"],
        ),
        (
            ColumnType::TimeTzPrecision(6),
            ["00:00:00+01", "12:00:00+00", "13:00:00+01"],
        ),
        (
            ColumnType::TimestampPrecision(6),
            [
                "1969-12-31 23:59:59.999999",
                "1970-01-01 00:00:00",
                "2024-02-29 12:00:00",
            ],
        ),
        (
            ColumnType::TimestampTzPrecision(6),
            [
                "1969-12-31 23:59:59+00",
                "2024-02-29 12:00:00+00",
                "2024-02-29 13:00:00+01",
            ],
        ),
        (ColumnType::Interval, ["-1 day", "1 month", "30 days"]),
    ] {
        let domain = ScalarIndexDomain::from_column_type(&ty).unwrap();
        let ScalarIndexDomain::Temporal(family) = domain else {
            panic!("expected temporal comparison domain");
        };
        let mut values = vec![Value::Null];
        values.extend(
            texts.map(|text| Value::Temporal(family.sample().parse_same_kind(text).unwrap())),
        );
        let mut targets = values.clone();
        targets.extend(texts.map(|text| Value::Str(text.into())));
        targets.push(Value::Str("not a temporal value".into()));
        let mut predicates = vec![Predicate::IsNull, Predicate::IsNotNull];
        for target in &targets {
            predicates.extend([
                Predicate::Equals(target.clone()),
                Predicate::GreaterThan(target.clone()),
                Predicate::GreaterThanOrEqual(target.clone()),
                Predicate::LessThan(target.clone()),
                Predicate::LessThanOrEqual(target.clone()),
            ]);
        }
        predicates.push(Predicate::InSet(targets.iter().cloned().collect()));
        for low in &targets {
            for high in &targets {
                predicates.push(Predicate::Between {
                    low: low.clone(),
                    high: high.clone(),
                });
            }
        }
        let empty = ColumnValueIndex::build("k", std::iter::empty());
        for predicate in &predicates {
            if let Some(postings) = empty.scan(predicate) {
                assert!(postings.is_empty());
            }
            for value in &values {
                assert_eq!(
                    observed(domain, predicate, value),
                    predicate.evaluate(Some(value)),
                    "{ty:?}, {predicate:?}, {value:?}"
                );
            }
        }
        let control = StorageReadControl::with_limit(1 << 20);
        for left in &values {
            for right in &values {
                assert_eq!(
                    domain
                        .encode(left, &control)
                        .unwrap()
                        .as_ref()
                        .cmp(domain.encode(right, &control).unwrap().as_ref()),
                    left.cmp(right)
                );
            }
        }
        let alias = ColumnType::Domain {
            schema: "public".into(),
            name: "clock_value".into(),
            oid: 42_001,
            base: Box::new(ty),
        };
        assert_eq!(ScalarIndexDomain::from_column_type(&alias), Some(domain));
    }
}

#[test]
fn temporal_text_parsing_shares_allowance_and_cancellation() {
    let domain = ScalarIndexDomain::Temporal(TemporalIndexDomain::Interval);
    let control = StorageReadControl::with_limit(64);
    let predicate = Predicate::Equals(Value::Str("1 month".into()));
    let mut visited = false;
    let error = domain
        .visit_predicate(&predicate, &control, &mut |_| {
            visited = true;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(!visited);
    assert_eq!(control.memory().used(), 0);
    let key = domain
        .encode(
            &Value::Temporal(TemporalIndexDomain::Interval.sample()),
            &control,
        )
        .unwrap();
    assert!(key.budget().shares_allowance(control.memory()));
    drop(key);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    let error = domain
        .visit_predicate(&predicate, &control, &mut |_| {
            visited = true;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(!visited);
    assert_eq!(control.memory().used(), 0);
}
