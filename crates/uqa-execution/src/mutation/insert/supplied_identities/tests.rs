//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cell::RefCell;

use uqa_storage::document_store::Document;

use super::*;
use crate::mutation::publication::{DocumentVectors, InsertedIdentity};

/// Records each observation and answers it with what the table's watermark is said to have been.
#[derive(Default)]
struct Observed {
    calls: RefCell<Vec<(String, DocId)>>,
    answers: BTreeMap<&'static str, ObservedIdentifier>,
}

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
        _inserted: InsertedIdentity,
    ) -> Result<(), SQLError> {
        unreachable!()
    }
    fn insert_document_deferred_text(
        &self,
        _table: &str,
        _doc_id: DocId,
        _document: Document,
        _vectors: DocumentVectors,
        _inserted: InsertedIdentity,
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
    fn observe_document_identity(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<ObservedIdentifier, SQLError> {
        self.calls.borrow_mut().push((table.to_owned(), doc_id));
        Ok(self
            .answers
            .get(table)
            .copied()
            .unwrap_or(ObservedIdentifier::Covered))
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
        *storage.calls.borrow(),
        [("public.a".to_owned(), 40), ("public.b".to_owned(), 3)]
    );
}

#[test]
fn a_statement_without_supplied_identities_observes_nothing() {
    let mut identities = SuppliedIdentities::default();
    identities.note("public.a", &insert(1, false));
    let storage = Observed::default();
    identities.observe(&storage).unwrap();
    assert!(storage.calls.borrow().is_empty());
}

#[test]
fn an_observation_tells_which_identities_no_document_ever_had() {
    let mut identities = SuppliedIdentities::default();
    for (table, doc_id) in [
        ("public.raised", 40),
        ("public.raised", 12),
        ("public.fresh", 3),
        ("public.covered", 9),
    ] {
        identities.note(table, &insert(doc_id, true));
    }
    let storage = Observed {
        answers: BTreeMap::from([
            (
                "public.raised",
                ObservedIdentifier::Observed { previous: Some(20) },
            ),
            (
                "public.fresh",
                ObservedIdentifier::Observed { previous: None },
            ),
            ("public.covered", ObservedIdentifier::Covered),
        ]),
        ..Observed::default()
    };
    let observed = identities.observe(&storage).unwrap();
    // Every use of an identity raised the watermark to it, so only an identity above the earlier watermark was never used.
    assert!(observed.unused("public.raised", 40));
    assert!(observed.unused("public.raised", 21));
    assert!(!observed.unused("public.raised", 20));
    assert!(!observed.unused("public.raised", 12));
    // A namespace without a watermark never held a document.
    assert!(observed.unused("public.fresh", 3));
    // A watermark the session had already read does not say where it stood before.
    assert!(!observed.unused("public.covered", 9));
    // Nor does a table the statement supplied no identity to.
    assert!(!observed.unused("public.other", 1));
    assert!(!ObservedIdentities::default().unused("public.raised", 40));
}
