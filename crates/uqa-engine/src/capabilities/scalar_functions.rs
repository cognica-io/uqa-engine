//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose scalar execution services from current session and catalog state.
use crate::{Engine, TableState};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::DocId;
use uqa_execution::query::{
    model_training::{ModelTrainingContext, TrainedModels, TrainingTable, TrainingTables},
    scalar_functions::{ScalarFunctionContext, ScalarSession},
    scalar_projection::AnalyzerRevisions,
};
use uqa_ml::DeepModel;
use uqa_sql::SQLError;
use uqa_storage::{document_store::Document, StorageBackendResult};
impl ScalarSession for Engine {
    fn backend_process_id(&self) -> i32 {
        self.backend_process_id()
    }
    fn notify(&self, channel: &str, payload: &str) -> Result<(), SQLError> {
        self.notify(channel, payload)
    }
    fn notification_queue_usage(&self) -> Result<f64, SQLError> {
        self.notification_queue_usage()
    }
}
impl AnalyzerRevisions for Engine {
    fn analyzer_revision(&self, name: &str) -> Result<Arc<uqa_analysis::CompiledAnalyzer>, String> {
        self.resolve_analyzer_revision(name)
    }
}

struct TrainingTableState(Arc<TableState>);

impl TrainingTable for TrainingTableState {
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.0.document_store.read().doc_ids()
    }
}

impl TrainingTables for Engine {
    fn training_table(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<Box<dyn TrainingTable + '_>>> {
        self.try_table(name).map(|table| {
            table.map(|state| Box::new(TrainingTableState(state)) as Box<dyn TrainingTable>)
        })
    }

    fn training_documents(
        &self,
        table: &str,
        doc_ids: &[DocId],
        projection: &[String],
    ) -> Result<BTreeMap<DocId, Document>, SQLError> {
        self.get_documents_with_materialized_projection(table, doc_ids, projection)
    }
}

impl TrainedModels for Engine {
    fn save_model(&self, name: &str, model: &DeepModel) -> Result<(), SQLError> {
        self.save_model(name, model)
    }
}

impl Engine {
    pub(crate) fn model_training_context(&self) -> ModelTrainingContext<'_> {
        ModelTrainingContext {
            tables: self,
            models: self,
        }
    }

    pub(crate) fn scalar_function_context(&self) -> ScalarFunctionContext<'_> {
        ScalarFunctionContext {
            catalog: self.catalog_execution(),
            sequences: self.sequence_introspection_context(),
            names: self,
            roles: self,
            database: self.database_privilege_inquiry(),
            schemas: self.schema_privilege_inquiry(),
            sequence_privileges: self.sequence_privilege_inquiry(),
            tables: self.table_privilege_context(),
            session: self,
            graphs: self,
            models: self.model_training_context(),
            analyzers: self,
            runtime: self.query_runtime_view(),
        }
    }
}
