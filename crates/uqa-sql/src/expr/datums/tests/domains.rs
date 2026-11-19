//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{expr::SQLValueCatalog, ColumnType};

struct Domain(ColumnType);
impl SQLValueCatalog for Domain {
    fn enum_type_labels(
        &self,
        _: u32,
    ) -> Result<Option<std::sync::Arc<crate::expr::enums::EnumTypeLabels>>, SQLError> {
        Ok(None)
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        false
    }

    fn value_type_by_oid_with_control(
        &self,
        oid: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        if oid == 50000 {
            Ok(Some(self.0.clone_with_control(control)?))
        } else {
            Ok(None)
        }
    }
}

fn domain(base: ColumnType) -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: "original".into(),
        oid: 50000,
        array_oid: Some(50001),
        collation_oid: None,
        base: Box::new(base),
    }
}

#[test]
fn physical_domain_reads_select_base_layout_before_interpreting_payload() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(4096);
    let control = ProductionControl::new(&memory, &token, &token);
    for (ty, bytes, expected) in [
        (
            ColumnType::Integer,
            1_i32.to_le_bytes().to_vec(),
            Value::Int(1),
        ),
        (
            domain(ColumnType::BigInteger),
            (-9_i64).to_le_bytes().to_vec(),
            Value::Int(-9),
        ),
        (
            ColumnType::Text,
            vec![16, 0, 0, 0],
            Value::Str(String::new()),
        ),
        (
            ColumnType::Text,
            vec![7, b'a', b'b'],
            Value::Str("ab".into()),
        ),
    ] {
        let catalog = Domain(domain(ty));
        let mut owned = vec![0; 8];
        owned.extend_from_slice(&bytes);
        let datum = DatumValue::new(50000, 8, owned);
        let before = datum.bytes().to_vec();
        let value = read_with_value_catalog_and_control(&datum, Some(&catalog), &control).unwrap();
        assert_eq!(*value, expected);
        assert_eq!(datum.type_oid(), 50000);
        assert_eq!(datum.offset(), 8);
        assert_eq!(datum.bytes(), before);
        drop(value);
        assert_eq!(memory.used(), 0);
    }
    let catalog = Domain(domain(ColumnType::Integer));
    let malformed = DatumValue::new(50000, 0, vec![1, 0]);
    assert_eq!(
        read_with_value_catalog_and_control(&malformed, Some(&catalog), &control)
            .unwrap_err()
            .sqlstate(),
        Some("XX001")
    );
    let denied = MemoryBudget::new(0);
    let datum = DatumValue::new(50000, 0, 1_i32.to_le_bytes().to_vec());
    assert_eq!(
        read_with_value_catalog_and_control(
            &datum,
            Some(&catalog),
            &ProductionControl::new(&denied, &token, &token)
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    token.cancel();
    assert!(read_with_value_catalog_and_control(&datum, Some(&catalog), &control).is_err());
    assert_eq!(memory.used(), 0);
    assert_eq!(denied.used(), 0);
}
