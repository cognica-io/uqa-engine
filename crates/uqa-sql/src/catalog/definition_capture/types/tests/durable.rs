//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn durable_types_exclude_temporary_names_and_authority_before_output_admission() {
    macro_rules! selected {
        ($capture:ident, $value:expr) => {{
            let original = $value;
            let key = original.identity.qualified_name();
            let mut source = BTreeMap::from([(key, original.clone())]);
            let memory = MemoryBudget::new(16384);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let mut saved = None;
            let baseline =
                allocation_counter::measure(|| saved = Some($capture(&source, &control).unwrap()));
            let bytes = memory.used();
            let expected = serde_json::to_value(&**saved.as_ref().unwrap()).unwrap();
            drop(saved.take());
            for schema in ["pg_temp_7", "pg_toast_temp_7"] {
                let mut temporary = original.clone();
                temporary.identity.schema = schema.into();
                temporary.identity.name = "x".repeat(1 << 20);
                temporary.usage_acl = Some(vec![acl().unwrap().remove(0); 16384]);
                source.insert(temporary.identity.qualified_name(), temporary);
            }
            let selected =
                allocation_counter::measure(|| saved = Some($capture(&source, &control).unwrap()));
            assert_eq!(selected.bytes_total, baseline.bytes_total);
            assert_eq!(selected.bytes_current, baseline.bytes_current);
            assert_eq!(memory.used(), bytes);
            assert_eq!(
                serde_json::to_value(&**saved.as_ref().unwrap()).unwrap(),
                expected
            );
            drop(saved);
            assert_eq!(memory.used(), 0);
            token.cancel();
            assert!(matches!(
                $capture(&source, &control),
                Err(SQLError::Cancelled(_))
            ));
            assert_eq!(memory.used(), 0);
        }};
    }
    selected!(domains, domain());
    selected!(enums, enumeration());
    selected!(composites, composite());
    selected!(ranges, range());
    selected!(shells, shell());
}

#[test]
fn durable_type_catalogs_reject_every_insufficient_allowance_and_release_partial_output() {
    macro_rules! bounded {
        ($capture:ident, $value:expr) => {{
            let value = $value;
            let source = BTreeMap::from([(value.identity.qualified_name(), value)]);
            let token = CancellationToken::new();
            let memory = MemoryBudget::new(16384);
            let captured =
                $capture(&source, &ProductionControl::new(&memory, &token, &token)).unwrap();
            let required = memory.used();
            drop(captured);
            for bytes in 0..required {
                let memory = MemoryBudget::new(bytes);
                assert_eq!(
                    $capture(&source, &ProductionControl::new(&memory, &token, &token))
                        .unwrap_err()
                        .sqlstate(),
                    Some("53200")
                );
                assert_eq!(memory.used(), 0);
            }
            let memory = MemoryBudget::new(required);
            let captured =
                $capture(&source, &ProductionControl::new(&memory, &token, &token)).unwrap();
            assert_eq!(memory.used(), required);
            drop(captured);
            assert_eq!(memory.used(), 0);
        }};
    }
    bounded!(domains, domain());
    bounded!(enums, enumeration());
    bounded!(composites, composite());
    bounded!(ranges, range());
    bounded!(shells, shell());
}
