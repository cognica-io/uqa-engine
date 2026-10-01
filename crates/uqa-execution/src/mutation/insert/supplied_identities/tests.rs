//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cell::RefCell;

use uqa_storage::document_store::Document;

use super::*;
use crate::mutation::publication::DocumentVectors;

#[derive(Default)]
struct Observed(RefCell<Vec<(String, DocId)>>);

impl MutationStorage for Observed {
    fn can_defer_document_text(&self, _table: &str) -> Result<bool, SQLError> {
        unreachable!()
    }
    fn delete_document(&self, _table: &str, _doc_id: DocId) -> Result<(), SQLError> {
        unreachable!()
    }
    fn delete_document_deferred_text(&self, _table: &str, _doc_id: DocId) -> Result<(), SQLError> {
        unreachable!()
    }
    fn insert_document(
        &self,
        _table: &str,
        _doc_id: DocId,
        _document: Document,
        _vectors: DocumentVectors,
        _known_new: bool,
    ) -> Result<(), SQLError> {
        unreachable!()
    }
    fn insert_document_deferred_text(
        &self,
        _table: &str,
        _doc_id: DocId,
        _document: Document,
        _vectors: DocumentVectors,
        _known_new: bool,
    ) -> Result<(), SQLError> {
        unreachable!()
    }
    fn rewrite_document(
        &self,
        _table: &str,
        _doc_id: DocId,
        _document: Document,
    ) -> Result<(), SQLError> {
        unreachable!()
    }
    fn rewrite_document_deferred_text(
        &self,
        _table: &str,
        _doc_id: DocId,
        _document: Document,
    ) -> Result<(), SQLError> {
        unreachable!()
    }
    fn observe_document_identity(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        self.0.borrow_mut().push((table.to_owned(), doc_id));
        Ok(())
    }
}

const fn insert(doc_id: DocId, supplied: bool) -> PreparedInsertConflict {
    PreparedInsertConflict::Insert { doc_id, supplied }
}

#[test]
fn each_table_observes_the_greatest_identity_its_rows_supply() {
    let mut identities = SuppliedIdentities::default();
    for (table, prepared) in [
        ("public.a", insert(7, true)),
        ("public.b", insert(3, true)),
        ("public.a", insert(40, true)),
        ("public.a", insert(12, true)),
        // A generated identity was reserved, which already raised the watermark past it.
        ("public.a", insert(900, false)),
        ("public.c", insert(5, false)),
        // A skipped or unresolved row publishes no identity.
        ("public.b", PreparedInsertConflict::Skip),
        ("public.d", PreparedInsertConflict::Unresolved),
    ] {
        identities.note(table, &prepared);
    }
    let storage = Observed::default();
    identities.observe(&storage).unwrap();
    assert_eq!(
        *storage.0.borrow(),
        [("public.a".to_owned(), 40), ("public.b".to_owned(), 3)]
    );
}

#[test]
fn a_statement_without_supplied_identities_observes_nothing() {
    let mut identities = SuppliedIdentities::default();
    identities.note("public.a", &insert(1, false));
    let storage = Observed::default();
    identities.observe(&storage).unwrap();
    assert!(storage.0.borrow().is_empty());
}
