//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Referential-action recursion, pending row images, and statement-trigger event state.

use std::collections::BTreeMap;

use uqa_core::DocId;
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use super::PhysicalDocumentIdentity;
use crate::Engine;

#[derive(Default)]
pub(in crate::sql) struct MutationEventQueue {
    after_rows: Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    referential_actions: ReferentialActionContext,
}

impl MutationEventQueue {
    pub(in crate::sql) fn after_rows(&self) -> &[crate::sql::triggers::AfterRowTriggerEvent] {
        &self.after_rows
    }

    pub(in crate::sql) fn after_rows_mut(
        &mut self,
    ) -> &mut Vec<crate::sql::triggers::AfterRowTriggerEvent> {
        &mut self.after_rows
    }

    pub(in crate::sql) fn append_after_rows(
        &mut self,
        events: Vec<crate::sql::triggers::AfterRowTriggerEvent>,
    ) {
        crate::sql::triggers::AfterRowTriggerEvent::append(&mut self.after_rows, events);
    }

    pub(in crate::sql) fn referential_actions_mut(&mut self) -> &mut ReferentialActionContext {
        &mut self.referential_actions
    }

    pub(in crate::sql) fn referential_transition_tables(
        &self,
        engine: &Engine,
    ) -> Result<Vec<crate::sql::triggers::TransitionTables>, SQLError> {
        self.referential_actions
            .transition_tables(engine, &self.after_rows)
    }

    pub(in crate::sql) fn fire_referential_after_statement_triggers(
        &self,
        engine: &Engine,
        transitions: &[crate::sql::triggers::TransitionTables],
        root_table: &str,
        root_events: &[uqa_sql::ast::TriggerEvent],
        generation: usize,
    ) -> Result<(), SQLError> {
        self.referential_actions.fire_after_statement_triggers(
            engine,
            transitions,
            root_table,
            root_events,
            generation,
        )
    }
}

#[derive(Default)]
pub(in crate::sql) struct ReferentialActionContext {
    pub(in crate::sql) delete_stack: Vec<(String, DocId)>,
    pub(in crate::sql) rewrite_stack: Vec<(String, DocId)>,
    pub(in crate::sql) trigger_statements: crate::sql::triggers::ReferentialTriggerStatements,
    pending_documents: BTreeMap<PhysicalDocumentIdentity, Option<Document>>,
}

impl ReferentialActionContext {
    pub(in crate::sql) fn pending_document(
        &self,
        identity: &PhysicalDocumentIdentity,
    ) -> Option<&Option<Document>> {
        self.pending_documents.get(identity)
    }

    pub(in crate::sql) fn record_pending_document(
        &mut self,
        identity: PhysicalDocumentIdentity,
        document: Option<Document>,
    ) {
        self.pending_documents.insert(identity, document);
    }

    pub(in crate::sql) fn transition_tables(
        &self,
        engine: &Engine,
        events: &[crate::sql::triggers::AfterRowTriggerEvent],
    ) -> Result<Vec<crate::sql::triggers::TransitionTables>, SQLError> {
        self.trigger_statements
            .build_transition_tables(engine, events)
    }

    pub(in crate::sql) fn fire_after_statement_triggers(
        &self,
        engine: &Engine,
        transitions: &[crate::sql::triggers::TransitionTables],
        root_table: &str,
        root_events: &[uqa_sql::ast::TriggerEvent],
        generation: usize,
    ) -> Result<(), SQLError> {
        self.trigger_statements
            .fire_after(engine, transitions, root_table, root_events, generation)
    }
}

pub(in crate::sql) struct ReferentialRewritePreparation<'a> {
    pub(in crate::sql) table: &'a str,
    pub(in crate::sql) doc_id: DocId,
    pub(in crate::sql) old_document: Document,
    pub(in crate::sql) proposed_document: Document,
    pub(in crate::sql) updated_columns: Vec<String>,
}
