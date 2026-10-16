//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue};

fn text_datum(text: &str) -> Value {
    let mut bytes = ((u32::try_from(text.len()).unwrap() + 4) << 2)
        .to_le_bytes()
        .to_vec();
    bytes.extend_from_slice(text.as_bytes());
    Value::Datum(DatumValue::new(25, 0, bytes))
}

#[test]
fn admitted_sql_keys_preserve_native_bytes_and_observe_physical_values() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(2 * 1024 * 1024);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let logical = vec![
        Value::Float(0.1),
        Value::Float(f64::from_bits(1)),
        Value::Decimal(DecimalValue::parse("123.45000").unwrap()),
        Value::JsonB("{\"z\":1.00,\"a\":[true,null,1e-8]}".into()),
        Value::List(vec![
            Value::Str("é-bound".repeat(8192)),
            Value::FixedChar("x   ".into()),
        ]),
    ];
    let mut physical = logical.clone();
    physical[4] = Value::List(vec![
        text_datum(&"é-bound".repeat(8192)),
        Value::FixedChar("x   ".into()),
    ]);
    let expected = encode_sql_key_with_enum_catalog(logical.iter().map(Some), None).unwrap();
    for values in [&logical, &physical] {
        let key =
            canonical_row_key_with_catalog_and_control(values.iter().map(Some), None, &control)
                .unwrap();
        assert_eq!(&*key, &expected);
        assert_eq!(budget.used(), key.capacity());
        drop(key);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn admitted_sql_key_failures_release_decoding_and_normalization_scratch() {
    let value = Value::List(vec![
        Value::JsonB("{\"number\":123456789.0000000001}".into()),
        text_datum(&"payload".repeat(8192)),
    ]);
    for limit in [0, 128, 1024, 65536] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = canonical_row_key_with_catalog_and_control(
            std::iter::once(Some(&value)),
            None,
            &control,
        )
        .map(drop)
        .expect_err("expected resource rejection");
        assert_eq!(error.sqlstate(), Some("53200"), "{limit}: {error}");
        assert_eq!(budget.used(), 0);
    }
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(2 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = canonical_row_key_with_catalog_and_control(
            std::iter::once(Some(&value)),
            None,
            &control,
        )
        .map(drop)
        .expect_err("expected resource rejection");
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

struct CancellingCatalog(CancellationToken);

impl uqa_sql::expr::SQLValueCatalog for CancellingCatalog {
    fn value_range_type(
        &self,
        _: u32,
    ) -> Result<Option<std::sync::Arc<uqa_sql::expr::RangeValueDescriptor>>, uqa_sql::SQLError>
    {
        self.0.cancel();
        Ok(None)
    }
    fn enum_type_labels(
        &self,
        _: u32,
    ) -> Result<Option<std::sync::Arc<uqa_sql::expr::enums::EnumTypeLabels>>, uqa_sql::SQLError>
    {
        Ok(None)
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, uqa_sql::SQLError> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        false
    }
}

#[test]
fn admitted_sql_key_catalog_interrupts_preserve_both_cancellation_scopes() {
    let Value::Datum(source) = text_datum(&"payload".repeat(8192)) else {
        unreachable!()
    };
    let value = Value::Datum(DatumValue::new(51_000, 0, source.bytes().to_vec()));
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(2 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let catalog = CancellingCatalog(if cancel_original {
            original.clone()
        } else {
            invoking.clone()
        });
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = canonical_row_key_with_catalog_and_control(
            std::iter::once(Some(&value)),
            Some(&catalog),
            &control,
        )
        .map(drop)
        .expect_err("expected resource rejection");
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
