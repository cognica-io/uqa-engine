//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_default, fold_sql_function_result, CreateFunction, FunctionBody,
    RoutineCompilationContext, RoutineCompilationMode, SQLBodyInputs, SQLError,
    StoredSourceCatalog,
};
use uqa_core::memory::ProductionControl;

mod fields;
use fields::{Fields, Inline, Native};

/// Supply the same ordered edits against an ordinary definition or independently admitted stored syntax.
pub trait RoutineDeclaration {
    fn declaration(&self) -> &CreateFunction;
    fn edit_statement(
        &mut self,
        index: usize,
        control: &ProductionControl<'_>,
        bind: impl FnOnce(&mut crate::ast::Statement) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError>;
    fn edit_parameter(
        &mut self,
        index: usize,
        control: &ProductionControl<'_>,
        bind: impl FnOnce(&mut crate::ast::FunctionParam) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError>;
}

impl RoutineDeclaration for CreateFunction {
    fn declaration(&self) -> &CreateFunction {
        self
    }
    fn edit_statement(
        &mut self,
        index: usize,
        _: &ProductionControl<'_>,
        bind: impl FnOnce(&mut crate::ast::Statement) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError> {
        let FunctionBody::Statements(values) = &mut self.body else {
            return Ok(None);
        };
        values.get_mut(index).map(bind).transpose()
    }
    fn edit_parameter(
        &mut self,
        index: usize,
        _: &ProductionControl<'_>,
        bind: impl FnOnce(&mut crate::ast::FunctionParam) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError> {
        self.params.get_mut(index).map(bind).transpose()
    }
}

enum Stage {
    LegacyStatements,
    LegacyDefaults,
    Signature,
    Statements,
    Result,
    Defaults,
}

/// Keep declaration diagnostics, each statement's analysis, the final result and default bindings in their original order. Catalog inputs belong to the current step.
pub struct RoutineDefinitionDependencies {
    mode: RoutineCompilationMode,
    stage: Stage,
    position: usize,
    inputs: Option<SQLBodyInputs>,
    changed: bool,
}

impl RoutineDefinitionDependencies {
    pub fn new(mode: RoutineCompilationMode) -> Self {
        Self {
            mode,
            stage: match mode {
                RoutineCompilationMode::Persisted => Stage::LegacyStatements,
                RoutineCompilationMode::Definition => Stage::Signature,
            },
            position: 0,
            inputs: None,
            changed: false,
        }
    }

    fn enter(&mut self, stage: Stage) {
        self.stage = stage;
        self.position = 0;
    }

    pub fn advance(
        &mut self,
        context: &RoutineCompilationContext<'_>,
        sources: &dyn StoredSourceCatalog,
        overloads: &crate::routines::resolution::RoutineOverloadContext<'_>,
        def: &mut impl RoutineDeclaration,
        control: &ProductionControl<'_>,
    ) -> Result<Option<bool>, SQLError> {
        crate::catalog::work::inline(self.advance_fields(
            &mut Inline {
                context,
                sources,
                overloads,
                def,
                control,
            },
            control,
        ))
    }

    /// Perform one dependency edit against the same admitted declaration and defining catalog that selected it.
    pub async fn advance_from<R>(
        &mut self,
        catalog: &R,
        def: &mut crate::routines::StoredRoutineDefinition,
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<Option<bool>, SQLError>
    where
        R: crate::routines::compilation::StoredRoutineCatalog
            + StoredSourceCatalog
            + crate::routines::resolution::RoutineOverloadCatalog
            + crate::plan::AggregateClassifier,
    {
        self.advance_fields(
            &mut Native {
                catalog,
                def,
                control,
                input,
            },
            control,
        )
        .await
    }

    async fn advance_fields(
        &mut self,
        fields: &mut impl Fields,
        control: &ProductionControl<'_>,
    ) -> Result<Option<bool>, SQLError> {
        control.check()?;
        match self.stage {
            Stage::LegacyStatements => {
                if let Some(changed) = fields.legacy_statement(self.position).await? {
                    self.changed |= changed;
                    self.position += 1;
                } else {
                    self.enter(Stage::LegacyDefaults);
                }
            }
            Stage::LegacyDefaults => {
                if let Some(changed) = fields.legacy_default(self.position).await? {
                    self.changed |= changed;
                    self.position += 1;
                } else {
                    self.enter(Stage::Signature);
                }
            }
            Stage::Signature => {
                if matches!(fields.declaration().body, FunctionBody::Statements(_)) {
                    self.inputs = Some(fields.signature(self.mode).await?);
                    self.enter(Stage::Statements);
                } else {
                    self.enter(Stage::Defaults);
                }
            }
            Stage::Statements => {
                let inputs = self.inputs.as_ref().expect("routine statement inputs");
                if let Some(changed) = fields.statement(self.position, inputs, self.mode).await? {
                    self.changed |= changed;
                    self.position += 1;
                } else {
                    self.enter(Stage::Result);
                }
            }
            Stage::Result => {
                let inputs = self.inputs.take().expect("routine result inputs");
                if let FunctionBody::Statements(statements) = &fields.declaration().body {
                    if let Some(last) = statements.len().checked_sub(1) {
                        self.changed |= fields.final_result(last, &inputs).await?;
                    }
                }
                self.enter(Stage::Defaults);
            }
            Stage::Defaults => {
                let Some(changed) = fields.default(self.position, self.mode).await? else {
                    return Ok(Some(self.changed));
                };
                self.changed |= changed;
                self.position += 1;
            }
        }
        Ok(None)
    }
}
