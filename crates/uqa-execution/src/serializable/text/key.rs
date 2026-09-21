//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Length-delimited exact terms, immutable fields and resource-controlled logical addresses.

use uqa_core::memory::BudgetedVec;
use uqa_sql::ast::ColumnDef;
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult, TokenTermKey};

pub(super) fn field(
    columns: &[ColumnDef],
    kind: u8,
    field: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    control.check()?;
    let mut key = BudgetedVec::new(control.memory());
    key.push(kind)?;
    crate::serializable::field::append(&mut key, columns, field, control)?;
    Ok(key)
}

pub(super) fn term(
    columns: &[ColumnDef],
    name: &str,
    term: &TokenTermKey,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    let mut key = field(columns, super::POSTING, name, control)?;
    key.extend_from_slice(&(term.as_bytes().len() as u64).to_be_bytes())?;
    for chunk in term.as_bytes().chunks(1024) {
        control.check()?;
        key.extend_from_slice(chunk)?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_errors::storage_error;

    #[test]
    fn term_ranges_do_not_alias_scalar_raw_utf16_or_embedded_zero_terms() {
        let control = StorageReadControl::with_limit(4096);
        let terms = [
            TokenTermKey::from_text("a"),
            TokenTermKey::from_text("aa"),
            TokenTermKey::from_text("a\0"),
            TokenTermKey::from_text("\u{fffd}"),
            TokenTermKey::from_bytes(vec![1, 0xd8, 0]).unwrap(),
            TokenTermKey::from_bytes(vec![1, 0xdc, 0]).unwrap(),
        ];
        for (i, left) in terms.iter().enumerate() {
            let left = term(&[], "body", left, &control).unwrap();
            for right in &terms[i + 1..] {
                let right = term(&[], "body", right, &control).unwrap();
                assert!(!left.starts_with(&right));
                assert!(!right.starts_with(&left));
            }
        }
        assert_eq!(control.memory().used(), 0);
    }

    #[test]
    fn text_fields_retain_column_incarnations_and_distinct_metadata_spaces() {
        let uqa_sql::Statement::CreateTable(mut table) =
            uqa_sql::compile("CREATE TABLE t (body TEXT)")
                .unwrap()
                .remove(0)
        else {
            panic!("expected table");
        };
        table.columns[0].object_id = Some([1; 16]);
        let control = StorageReadControl::with_limit(4096);
        let token = TokenTermKey::from_text("term");
        let old = term(&table.columns, "body", &token, &control).unwrap();
        table.columns[0].name = "renamed".into();
        assert_eq!(
            &*old,
            &*term(&table.columns, "renamed", &token, &control).unwrap()
        );
        table.columns[0].object_id = Some([2; 16]);
        assert_ne!(
            &*old,
            &*term(&table.columns, "renamed", &token, &control).unwrap()
        );
        for kind in [super::super::DOCUMENT, super::super::STATISTICS] {
            assert!(!old.starts_with(&field(&table.columns, kind, "renamed", &control).unwrap()));
        }
        for name in ["body", "body\0", "bodyy", "renamed"] {
            assert_ne!(&*old, &*term(&[], name, &token, &control).unwrap());
        }
        table.columns[0].object_id = None;
        assert!(term(&table.columns, "renamed", &token, &control).is_err());
    }

    #[test]
    fn text_key_failures_preserve_the_original_allowance_and_cancellation() {
        let control = StorageReadControl::with_limit(128);
        let retained = term(&[], "body", &TokenTermKey::from_text("a"), &control).unwrap();
        let held = control.memory().used();
        let error = term(
            &[],
            "body",
            &TokenTermKey::from_text(&"a".repeat(4096)),
            &control,
        )
        .unwrap_err();
        assert_eq!(storage_error("bind text", &error).sqlstate(), Some("53200"));
        assert_eq!(control.memory().used(), held);
        control.cancellation().cancel();
        let error = field(&[], super::super::POSTING, "body", &control).unwrap_err();
        assert_eq!(storage_error("bind text", &error).sqlstate(), Some("57014"));
        drop(retained);
        assert_eq!(control.memory().used(), 0);
    }
}
