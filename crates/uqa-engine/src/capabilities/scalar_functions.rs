//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose scalar execution services from current session and catalog state.
use crate::Engine;
use uqa_execution::query::{
    model_training::ModelTraining,
    scalar_functions::{ScalarFunctionContext, ScalarSession},
};
use uqa_ml::{DeepLearnOutput, LearnOptions};
use uqa_sql::SQLError;
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
impl ModelTraining for Engine {
    fn train_json(
        &self,
        model: &str,
        source: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        self.deep_learn_json(model, source, options)
    }
    fn train_table(
        &self,
        model: &str,
        source: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        self.deep_learn_table(model, source, options)
    }
}
impl Engine {
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
            models: self,
        }
    }
}
