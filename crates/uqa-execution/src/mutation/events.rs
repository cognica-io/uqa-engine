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

use crate::mutation::candidate::PhysicalDocumentIdentity;
use crate::mutation::triggers::context::TriggerContext;

#[derive(Default)]
pub struct MutationEventQueue {
    after_rows: Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
    referential_actions: ReferentialActionContext,
}

impl MutationEventQueue {
    pub fn after_rows(&self) -> &[crate::mutation::triggers::AfterRowTriggerEvent] {
        &self.after_rows
    }

    pub fn after_rows_mut(&mut self) -> &mut Vec<crate::mutation::triggers::AfterRowTriggerEvent> {
        &mut self.after_rows
    }

    pub fn append_after_rows(
        &mut self,
        events: Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
    ) {
        crate::mutation::triggers::AfterRowTriggerEvent::append(&mut self.after_rows, events);
    }

    pub fn referential_actions_mut(&mut self) -> &mut ReferentialActionContext {
        &mut self.referential_actions
    }

    pub fn referential_transition_tables(
        &self,
        context: &TriggerContext<'_>,
    ) -> Result<Vec<crate::mutation::triggers::TransitionTables>, SQLError> {
        self.referential_actions
            .transition_tables(context, &self.after_rows)
    }

    pub fn fire_referential_after_statement_triggers(
        &self,
        context: &TriggerContext<'_>,
        transitions: &[crate::mutation::triggers::TransitionTables],
        root_table: &str,
        root_events: &[uqa_sql::ast::TriggerEvent],
        generation: usize,
    ) -> Result<(), SQLError> {
        self.referential_actions.fire_after_statement_triggers(
            context,
            transitions,
            root_table,
            root_events,
            generation,
        )
    }
}

#[derive(Default)]
pub struct ReferentialActionContext {
    pub delete_stack: Vec<(String, DocId)>,
    pub rewrite_stack: Vec<(String, DocId)>,
    pub trigger_statements: crate::mutation::triggers::ReferentialTriggerStatements,
    pending_documents: BTreeMap<PhysicalDocumentIdentity, Option<Document>>,
}

impl ReferentialActionContext {
    pub fn pending_document(
        &self,
        identity: &PhysicalDocumentIdentity,
    ) -> Option<&Option<Document>> {
        self.pending_documents.get(identity)
    }

    pub fn record_pending_document(
        &mut self,
        identity: PhysicalDocumentIdentity,
        document: Option<Document>,
    ) {
        self.pending_documents.insert(identity, document);
    }

    pub fn transition_tables(
        &self,
        context: &TriggerContext<'_>,
        events: &[crate::mutation::triggers::AfterRowTriggerEvent],
    ) -> Result<Vec<crate::mutation::triggers::TransitionTables>, SQLError> {
        self.trigger_statements
            .build_transition_tables(context, events)
    }

    pub fn fire_after_statement_triggers(
        &self,
        context: &TriggerContext<'_>,
        transitions: &[crate::mutation::triggers::TransitionTables],
        root_table: &str,
        root_events: &[uqa_sql::ast::TriggerEvent],
        generation: usize,
    ) -> Result<(), SQLError> {
        self.trigger_statements.fire_after(
            context,
            transitions,
            root_table,
            root_events,
            generation,
        )
    }
}

pub struct ReferentialRewritePreparation<'a> {
    pub table: &'a str,
    pub doc_id: DocId,
    pub old_document: Document,
    pub proposed_document: Document,
    pub updated_columns: Vec<String>,
}
