//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The dependency scheduler selects the same declaration fields for an ordinary definition and a retained private session.

use super::{
    bind_default, fold_sql_function_result, CreateFunction, RoutineCompilationContext,
    RoutineCompilationMode, RoutineDeclaration, SQLBodyInputs, SQLError, StoredSourceCatalog,
};
use crate::routines::{
    compilation::StoredRoutineCatalog,
    resolution::{RoutineOverloadCatalog, RoutineOverloadContext},
    StoredRoutineDefinition,
};
use std::future::{ready, Future};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};

pub(super) trait Fields {
    fn declaration(&self) -> &CreateFunction;
    fn legacy_statement(
        &mut self,
        index: usize,
    ) -> impl Future<Output = Result<Option<bool>, SQLError>> + Send;
    fn legacy_default(
        &mut self,
        index: usize,
    ) -> impl Future<Output = Result<Option<bool>, SQLError>> + Send;
    fn signature(
        &mut self,
        mode: RoutineCompilationMode,
    ) -> impl Future<Output = Result<SQLBodyInputs, SQLError>> + Send;
    fn statement(
        &mut self,
        index: usize,
        inputs: &SQLBodyInputs,
        mode: RoutineCompilationMode,
    ) -> impl Future<Output = Result<Option<bool>, SQLError>> + Send;
    fn final_result(
        &mut self,
        index: usize,
        inputs: &SQLBodyInputs,
    ) -> impl Future<Output = Result<bool, SQLError>> + Send;
    fn default(
        &mut self,
        index: usize,
        mode: RoutineCompilationMode,
    ) -> impl Future<Output = Result<Option<bool>, SQLError>> + Send;
}

pub(super) struct Inline<'a, 'c, 'd, D> {
    pub(super) context: &'a RoutineCompilationContext<'c>,
    pub(super) sources: &'a dyn StoredSourceCatalog,
    pub(super) overloads: &'a RoutineOverloadContext<'d>,
    pub(super) def: &'a mut D,
    pub(super) control: &'a ProductionControl<'a>,
}

impl<D: RoutineDeclaration> Fields for Inline<'_, '_, '_, D> {
    fn declaration(&self) -> &CreateFunction {
        self.def.declaration()
    }
    fn legacy_statement(
        &mut self,
        index: usize,
    ) -> impl Future<Output = Result<Option<bool>, SQLError>> + Send {
        ready(self.def.edit_statement(index, self.control, |statement| {
            crate::binding::legacy_patterns::restore_statement(statement, self.context.routines)
        }))
    }
    fn legacy_default(
        &mut self,
        index: usize,
    ) -> impl Future<Output = Result<Option<bool>, SQLError>> + Send {
        ready(self.def.edit_parameter(index, self.control, |parameter| {
            parameter.default.as_mut().map_or(Ok(false), |default| {
                crate::binding::legacy_patterns::restore_expression(default, self.context.routines)
                    .map_err(SQLError::Internal)
            })
        }))
    }
    fn signature(
        &mut self,
        mode: RoutineCompilationMode,
    ) -> impl Future<Output = Result<SQLBodyInputs, SQLError>> + Send {
        ready(SQLBodyInputs::new(
            self.context,
            self.def.declaration(),
            mode,
            self.control,
        ))
    }
    fn statement(
        &mut self,
        index: usize,
        inputs: &SQLBodyInputs,
        mode: RoutineCompilationMode,
    ) -> impl Future<Output = Result<Option<bool>, SQLError>> + Send {
        ready(self.def.edit_statement(index, self.control, |statement| {
            inputs.bind(self.context, self.sources, self.overloads, statement, mode)
        }))
    }
    fn final_result(
        &mut self,
        index: usize,
        inputs: &SQLBodyInputs,
    ) -> impl Future<Output = Result<bool, SQLError>> + Send {
        ready(
            self.def
                .edit_statement(index, self.control, |statement| {
                    fold_sql_function_result(self.context, &inputs.result_types, statement)
                })
                .map(|changed| changed.expect("selected final SQL statement")),
        )
    }
    fn default(
        &mut self,
        index: usize,
        mode: RoutineCompilationMode,
    ) -> impl Future<Output = Result<Option<bool>, SQLError>> + Send {
        ready(self.def.edit_parameter(index, self.control, |parameter| {
            bind_default(self.context, parameter, mode)
        }))
    }
}

pub(super) struct Native<'a, 'i, R> {
    pub(super) catalog: &'a R,
    pub(super) def: &'a mut StoredRoutineDefinition,
    pub(super) control: &'a ProductionControl<'a>,
    pub(super) input: &'a mut AsyncImageInput<'i>,
}

impl<
        R: StoredRoutineCatalog
            + StoredSourceCatalog
            + RoutineOverloadCatalog
            + crate::plan::AggregateClassifier,
    > Fields for Native<'_, '_, R>
{
    fn declaration(&self) -> &CreateFunction {
        self.def.declaration()
    }
    async fn legacy_statement(&mut self, index: usize) -> Result<Option<bool>, SQLError> {
        let Some(mut field) = self
            .def
            .statement_edit_from(index, self.control, self.input)
            .await?
        else {
            return Ok(None);
        };
        let changed = crate::binding::legacy_patterns::restore_statement_from(
            field.value_mut(),
            self.catalog,
            self.control,
            self.input,
        )
        .await?;
        field
            .finish_from(changed, self.control, self.input)
            .await
            .map(Some)
    }
    async fn legacy_default(&mut self, index: usize) -> Result<Option<bool>, SQLError> {
        let Some(mut field) = self
            .def
            .parameter_edit_from(index, self.control, self.input)
            .await?
        else {
            return Ok(None);
        };
        let changed = match field.value_mut().default.as_mut() {
            Some(default) => {
                crate::binding::legacy_patterns::restore_expression_from(
                    default,
                    self.catalog,
                    self.control,
                    self.input,
                )
                .await?
            }
            None => false,
        };
        field
            .finish_from(changed, self.control, self.input)
            .await
            .map(Some)
    }
    async fn signature(&mut self, mode: RoutineCompilationMode) -> Result<SQLBodyInputs, SQLError> {
        crate::routines::declaration::validate_stored_routine_signature_from(
            self.catalog,
            self.def.declaration(),
            self.control,
            self.input,
        )
        .await?;
        SQLBodyInputs::from_signature(
            &RoutineCompilationContext::from_catalog(self.catalog),
            self.def.declaration(),
            mode,
            self.control,
        )
    }
    async fn statement(
        &mut self,
        index: usize,
        inputs: &SQLBodyInputs,
        mode: RoutineCompilationMode,
    ) -> Result<Option<bool>, SQLError> {
        let Some(mut field) = self
            .def
            .statement_edit_from(index, self.control, self.input)
            .await?
        else {
            return Ok(None);
        };
        let changed = inputs
            .bind_from(
                self.catalog,
                field.value_mut(),
                mode,
                self.control,
                self.input,
            )
            .await?;
        field
            .finish_from(*changed, self.control, self.input)
            .await
            .map(Some)
    }
    async fn final_result(
        &mut self,
        index: usize,
        inputs: &SQLBodyInputs,
    ) -> Result<bool, SQLError> {
        let mut field = self
            .def
            .statement_edit_from(index, self.control, self.input)
            .await?
            .expect("selected final SQL statement");
        let changed = fold_sql_function_result(
            &RoutineCompilationContext::from_catalog(self.catalog),
            &inputs.result_types,
            field.value_mut(),
        )?;
        field.finish_from(changed, self.control, self.input).await
    }
    async fn default(
        &mut self,
        index: usize,
        mode: RoutineCompilationMode,
    ) -> Result<Option<bool>, SQLError> {
        crate::routines::dependencies::bind_stored_parameter_default_from(
            self.catalog,
            self.def,
            index,
            mode,
            self.control,
            self.input,
        )
        .await
    }
}
