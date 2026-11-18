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
        control.check()?;
        match self.stage {
            Stage::LegacyStatements => {
                if let Some(changed) = def.edit_statement(self.position, control, |statement| {
                    crate::binding::legacy_patterns::restore_statement(statement, context.routines)
                })? {
                    self.changed |= changed;
                    self.position += 1;
                } else {
                    self.enter(Stage::LegacyDefaults);
                }
            }
            Stage::LegacyDefaults => {
                if let Some(changed) = def.edit_parameter(self.position, control, |parameter| {
                    parameter.default.as_mut().map_or(Ok(false), |default| {
                        crate::binding::legacy_patterns::restore_expression(
                            default,
                            context.routines,
                        )
                        .map_err(SQLError::Internal)
                    })
                })? {
                    self.changed |= changed;
                    self.position += 1;
                } else {
                    self.enter(Stage::Signature);
                }
            }
            Stage::Signature => {
                if matches!(def.declaration().body, FunctionBody::Statements(_)) {
                    self.inputs = Some(SQLBodyInputs::new(
                        context,
                        def.declaration(),
                        self.mode,
                        control,
                    )?);
                    self.enter(Stage::Statements);
                } else {
                    self.enter(Stage::Defaults);
                }
            }
            Stage::Statements => {
                let inputs = self.inputs.as_ref().expect("routine statement inputs");
                if let Some(changed) = def.edit_statement(self.position, control, |statement| {
                    inputs.bind(context, sources, overloads, statement, self.mode)
                })? {
                    self.changed |= changed;
                    self.position += 1;
                } else {
                    self.enter(Stage::Result);
                }
            }
            Stage::Result => {
                let inputs = self.inputs.take().expect("routine result inputs");
                if let FunctionBody::Statements(statements) = &def.declaration().body {
                    if let Some(last) = statements.len().checked_sub(1) {
                        self.changed |= def
                            .edit_statement(last, control, |statement| {
                                fold_sql_function_result(context, &inputs.result_types, statement)
                            })?
                            .expect("selected final SQL statement");
                    }
                }
                self.enter(Stage::Defaults);
            }
            Stage::Defaults => {
                let Some(changed) = def.edit_parameter(self.position, control, |parameter| {
                    bind_default(context, parameter, self.mode)
                })?
                else {
                    return Ok(Some(self.changed));
                };
                self.changed |= changed;
                self.position += 1;
            }
        }
        Ok(None)
    }
}
