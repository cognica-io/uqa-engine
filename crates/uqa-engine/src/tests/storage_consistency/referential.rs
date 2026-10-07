//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The retained mutation adapters must keep indexed referential reads independent of unrelated payloads.

use super::*;
use std::{cell::Cell, path::Path};
use uqa_execution::mutation::constraints::context::MutationRead;
use uqa_execution::mutation::referential::referencing_rows;
use uqa_sql::ast::ForeignKeyAction;
use uqa_storage::read_control::StorageReadControl;

struct Reads<'a> {
    engine: &'a Engine,
    enumerations: Cell<usize>,
    documents: Cell<usize>,
    payload_bytes: Cell<usize>,
}

impl MutationRead for Reads<'_> {
    fn table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        self.enumerations.set(self.enumerations.get() + 1);
        MutationRead::table_doc_ids(self.engine, table)
    }

    fn live_table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        MutationRead::live_table_doc_ids(self.engine, table)
    }

    fn live_table_doc_id_page(
        &self,
        table: &str,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
        MutationRead::live_table_doc_id_page(self.engine, table, after, limit, control)
    }

    fn get_document(&self, table: &str, id: DocId) -> Result<Option<Document>, SQLError> {
        self.documents.set(self.documents.get() + 1);
        let document = MutationRead::get_document(self.engine, table, id)?;
        if let Some(Value::Str(body)) = document.as_ref().and_then(|row| row.get("body")) {
            self.payload_bytes
                .set(self.payload_bytes.get() + body.len());
        }
        Ok(document)
    }

    fn raw_document(&self, table: &str, id: DocId) -> Result<Option<Document>, SQLError> {
        MutationRead::raw_document(self.engine, table, id)
    }

    fn command_overlay_changes(
        &self,
        table: &str,
    ) -> Result<Option<uqa_execution::query::document_changes::DocumentChanges>, SQLError> {
        MutationRead::command_overlay_changes(self.engine, table)
    }
}

fn memory(_: &Path) -> Engine {
    Engine::new()
}

fn sqlite(path: &Path) -> Engine {
    Engine::open(path).unwrap()
}

fn sqlite_key_value(path: &Path) -> Engine {
    Engine::from_persistent_provider(Arc::new(
        uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
    ))
    .unwrap()
}

fn redb(path: &Path) -> Engine {
    Engine::from_persistent_provider(Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap()))
        .unwrap()
}

#[rstest::rstest]
#[case::memory(memory)]
#[case::sqlite(sqlite)]
#[case::sqlite_key_value(sqlite_key_value)]
#[case::redb(redb)]
fn indexed_references_read_only_matching_child_payloads(#[case] open: fn(&Path) -> Engine) {
    let mut failures = Vec::new();
    for unrelated in [16, 64] {
        for payload in [100, 20_000] {
            for action in [ForeignKeyAction::Cascade, ForeignKeyAction::NoAction] {
                let directory = tempfile::tempdir().unwrap();
                let engine = open(&directory.path().join("references.db"));
                let action_sql = if action == ForeignKeyAction::Cascade {
                    "ON DELETE CASCADE"
                } else {
                    ""
                };
                engine.sql(&format!(
                    "CREATE TABLE parent (id text PRIMARY KEY); \
                     CREATE TABLE child (parent_id text REFERENCES parent(id) {action_sql}, kind integer, body text NOT NULL, PRIMARY KEY (parent_id, kind)); \
                     INSERT INTO parent VALUES ('target'), ('empty'); \
                     INSERT INTO parent SELECT 'other-' || i::text FROM generate_series(1, {unrelated}) AS g(i); \
                     INSERT INTO child SELECT id, k, repeat('x', {payload}) FROM parent CROSS JOIN generate_series(1, 2) AS kinds(k) WHERE id <> 'empty'"
                ), &[]).unwrap();
                let foreign_key = engine
                    .foreign_keys_in_execution("public.child")
                    .unwrap()
                    .remove(0);
                let comparison = uqa_sql::semantics::foreign_keys::foreign_key_comparison_types(
                    &engine,
                    "public.child",
                    &foreign_key,
                )
                .unwrap();
                for (key, matches) in [("target", 2), ("empty", 0)] {
                    let reads = Reads {
                        engine: &engine,
                        enumerations: Cell::new(0),
                        documents: Cell::new(0),
                        payload_bytes: Cell::new(0),
                    };
                    let mut context = engine.referential_execution_context();
                    context.constraints.reads = &reads;
                    let found = referencing_rows(
                        &context,
                        "public.child",
                        &foreign_key,
                        &comparison,
                        &[s(key)],
                        action,
                    )
                    .unwrap();
                    assert_eq!(found.len(), matches);
                    if reads.enumerations.get() != 0 || reads.documents.get() != matches {
                        failures.push(format!(
                            "{action:?}, unrelated={unrelated}, payload={payload}, key={key}: enumerations={}, documents={}, payload_bytes={}, matches={matches}",
                            reads.enumerations.get(), reads.documents.get(), reads.payload_bytes.get(),
                        ));
                    }
                }
                let delete = engine.sql("DELETE FROM parent WHERE id = 'target'", &[]);
                if action == ForeignKeyAction::NoAction {
                    assert_eq!(delete.unwrap_err().sqlstate(), Some("23503"));
                    engine.sql("DELETE FROM child WHERE parent_id = 'target'; DELETE FROM parent WHERE id = 'target'", &[]).unwrap();
                } else {
                    delete.unwrap();
                }
                engine
                    .sql("DELETE FROM parent WHERE id = 'empty'", &[])
                    .unwrap();
                assert_eq!(
                    engine
                        .sql("SELECT count(*) AS n FROM child", &[])
                        .unwrap()
                        .rows[0]["n"],
                    Value::Int(unrelated * 2)
                );
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
