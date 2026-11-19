//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture routine namespaces, bind catalog dependencies, and recompile changed definitions.

use super::compilation::{self, StoredRoutineCompilationContext};
use std::sync::Arc;
use uqa_core::memory::{MemoryReservation, ProductionControl, ProductionVec};
use uqa_sql::{
    ast::{CreateFunction, FunctionBody},
    binding::stored_columns::StoredSourceCatalog,
    routines::{
        compilation::{
            compile_function_body, defer_function_body, with_parser_context,
            SQLStandardBodyCompilation,
        },
        dependencies::{RoutineCompilationMode, RoutineDeclaration, RoutineDefinitionDependencies},
        regclass::{RoutineRegclassBinding, RoutineRegclassCatalog},
        CompiledFunctionBody, RoutineBody, StoredRoutineDefinition, StoredStatementCompilation,
    },
    semantics::sets::placement_scope::SetPlacementScope,
    SQLError,
};

pub struct RoutineDefinitionContext<'a> {
    pub compilation: StoredRoutineCompilationContext<'a>,
    pub sources: &'a dyn StoredSourceCatalog,
    pub overloads: &'a dyn uqa_sql::routines::resolution::RoutineOverloadCatalog,
    pub regclasses: &'a dyn RoutineRegclassCatalog,
}

/// How a routine's body is compiled with its definition. A SQL-standard body belongs to the statement that creates the routine, so it is always compiled and bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutineBodyCompilation {
    /// `CREATE FUNCTION` under `check_function_bodies`: a body given as a string is compiled under the routine's own settings, and its errors are reported.
    Checked,
    /// `CREATE FUNCTION` with `check_function_bodies` off: a body given as a string is stored unexamined.
    Unchecked,
    /// A stored definition: a body given as a string is left for each session to compile when it first calls the routine, as each `PostgreSQL` backend compiles a function at its first call, so that a body that no longer compiles reports its error then.
    Stored,
}

impl RoutineBodyCompilation {
    const fn mode(self) -> RoutineCompilationMode {
        match self {
            Self::Checked | Self::Unchecked => RoutineCompilationMode::Definition,
            Self::Stored => RoutineCompilationMode::Persisted,
        }
    }
}

/// A routine definition bound to the catalog.
pub struct BoundRoutine {
    /// The body as the catalog keeps it.
    pub body: RoutineBody,
    /// The compilation that validated a source body when the routine was defined.
    pub validated: Option<CompiledFunctionBody>,
    /// Whether binding changed the stored definition.
    pub changed: bool,
}

pub fn compile_catalog_bound_routine(
    context: &RoutineDefinitionContext<'_>,
    def: &mut CreateFunction,
    bodies: RoutineBodyCompilation,
) -> Result<BoundRoutine, SQLError> {
    if matches!(bodies.mode(), RoutineCompilationMode::Definition) {
        if matches!(def.body, FunctionBody::Statements(_))
            || def
                .params
                .iter()
                .any(|parameter| parameter.default.is_some())
        {
            def.creation_search_path = context.compilation.session.routine_search_path();
        } else {
            def.creation_search_path.clear();
        }
    }
    let control = ProductionControl::uncontrolled();
    let mut binding = CatalogRoutineBinding::new(def, bodies, &control)?;
    loop {
        if let Some(bound) = binding.advance(context, def, &control)? {
            return Ok(bound);
        }
    }
}

enum Stage {
    Dependencies(RoutineDefinitionDependencies),
    Compile,
    Statements(SQLStandardBodyCompilation),
    Regclasses(RoutineRegclassBinding),
    Finished,
}

pub(super) enum BindingStep {
    Pending,
    Statement(StoredStatementCompilation),
    Complete(BoundRoutine),
}

enum StatementStep {
    Progress(bool),
    Lower(StoredStatementCompilation),
}

/// Catalog compilation retains its declaration order across calls. Session, parser and set-placement scopes exist only within the selected step.
pub struct CatalogRoutineBinding {
    bodies: RoutineBodyCompilation,
    stage: Stage,
    search_path: Arc<Vec<String>>,
    _path_memory: Option<MemoryReservation>,
    changed: bool,
    recompile: bool,
    compiled: Option<RoutineBody>,
    validated: Option<CompiledFunctionBody>,
}

impl CatalogRoutineBinding {
    pub fn new(
        def: &CreateFunction,
        bodies: RoutineBodyCompilation,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut path = ProductionVec::new(*control);
        path.reserve(def.creation_search_path.len())?;
        for name in &def.creation_search_path {
            path.push_produced(control.copy_text(name)?)?;
        }
        let (path, memory) = path.finish()?.into_parts();
        let memory = control.combine(memory, control.reserve(size_of::<Vec<String>>())?);
        Ok(Self {
            bodies,
            stage: Stage::Dependencies(RoutineDefinitionDependencies::new(bodies.mode())),
            search_path: Arc::new(path),
            _path_memory: memory,
            changed: false,
            recompile: false,
            compiled: None,
            validated: None,
        })
    }

    pub fn advance(
        &mut self,
        context: &RoutineDefinitionContext<'_>,
        def: &mut impl RoutineDeclaration,
        control: &ProductionControl<'_>,
    ) -> Result<Option<BoundRoutine>, SQLError> {
        match self.advance_with(context, def, control, |input, def, analysis| {
            let FunctionBody::Statements(statements) = &def.declaration().body else {
                unreachable!("SQL-standard compilation")
            };
            input
                .advance(analysis, statements, control)
                .map(StatementStep::Progress)
        })? {
            BindingStep::Pending => Ok(None),
            BindingStep::Complete(bound) => Ok(Some(bound)),
            BindingStep::Statement(_) => unreachable!("ordinary statements complete inline"),
        }
    }

    pub(super) fn advance_restored(
        &mut self,
        context: &RoutineDefinitionContext<'_>,
        def: &mut StoredRoutineDefinition,
        lowered: Option<uqa_sql::plan::RetainedExecutablePlan>,
        control: &ProductionControl<'_>,
    ) -> Result<BindingStep, SQLError> {
        self.advance_with(context, def, control, |input, def, analysis| {
            if let Some(plan) = lowered {
                input.accept_retained(analysis, plan, control)?;
                return Ok(StatementStep::Progress(false));
            }
            if let Some(position) = input.next_statement() {
                return def
                    .compilation_statement(position, analysis.merge, control)
                    .map(StatementStep::Lower);
            }
            let FunctionBody::Statements(statements) = &def.declaration().body else {
                unreachable!("SQL-standard compilation")
            };
            input
                .advance(analysis, statements, control)
                .map(StatementStep::Progress)
        })
    }

    fn advance_with<D: RoutineDeclaration>(
        &mut self,
        context: &RoutineDefinitionContext<'_>,
        def: &mut D,
        control: &ProductionControl<'_>,
        statement: impl FnOnce(
            &mut SQLStandardBodyCompilation,
            &mut D,
            &uqa_sql::routines::compilation::RoutineCompilationContext<'_>,
        ) -> Result<StatementStep, SQLError>,
    ) -> Result<BindingStep, SQLError> {
        control.check()?;
        let stored = self.bodies == RoutineBodyCompilation::Stored;
        let _placement = stored.then(SetPlacementScope::stored_definition);
        let needs_path = match self.stage {
            Stage::Regclasses(_) => true,
            Stage::Dependencies(_) => !self.search_path.is_empty(),
            Stage::Compile | Stage::Statements(_) => {
                stored
                    && !self.search_path.is_empty()
                    && matches!(def.declaration().body, FunctionBody::Statements(_))
            }
            Stage::Finished => false,
        };
        let session = context.compilation.session;
        let _path = needs_path.then(|| SearchPath {
            session,
            previous: Some(session.replace_shared_routine_search_path(self.search_path.clone())),
        });
        let analysis = &context.compilation.analysis;
        match &mut self.stage {
            Stage::Dependencies(input) => {
                if let Some(changed) = input.advance(
                    analysis,
                    context.sources,
                    &uqa_sql::routines::resolution::RoutineOverloadContext {
                        catalog: context.overloads,
                    },
                    def,
                    control,
                )? {
                    self.changed |= changed;
                    self.stage = Stage::Compile;
                }
            }
            Stage::Compile => {
                let def = def.declaration();
                if matches!(def.body, FunctionBody::Statements(_)) {
                    self.stage = Stage::Statements(with_parser_context(analysis.parsers, || {
                        SQLStandardBodyCompilation::new(analysis, def, stored, control)
                    })?);
                } else {
                    self.validated = match self.bodies {
                        RoutineBodyCompilation::Checked => Some(
                            compilation::with_routine_settings(&context.compilation, def, || {
                                compile_function_body(analysis, def)
                            })?,
                        ),
                        RoutineBodyCompilation::Unchecked => defer_function_body(analysis, def)?,
                        RoutineBodyCompilation::Stored => None,
                    };
                    self.compiled = Some(RoutineBody::Source);
                    self.compiled_stage();
                }
            }
            Stage::Statements(input) => {
                match with_parser_context(analysis.parsers, || statement(input, def, analysis))? {
                    StatementStep::Lower(source) => return Ok(BindingStep::Statement(source)),
                    StatementStep::Progress(false) => {}
                    StatementStep::Progress(true) => {
                        let Stage::Statements(input) =
                            std::mem::replace(&mut self.stage, Stage::Finished)
                        else {
                            unreachable!()
                        };
                        self.compiled = Some(RoutineBody::Bound(input.into_shared(control)?));
                        self.compiled_stage();
                    }
                }
            }
            Stage::Regclasses(input) => {
                if let Some(changed) =
                    input.advance(analysis.types, context.regclasses, def, control)?
                {
                    self.changed |= changed;
                    self.recompile = changed;
                    if changed {
                        self.compiled = None;
                        self.validated = None;
                        self.stage = Stage::Compile;
                    } else {
                        self.stage = Stage::Finished;
                    }
                }
            }
            Stage::Finished => {
                return Ok(BindingStep::Complete(BoundRoutine {
                    body: self.compiled.take().expect("compiled routine"),
                    validated: self.validated.take(),
                    changed: self.changed,
                }))
            }
        }
        Ok(BindingStep::Pending)
    }

    fn compiled_stage(&mut self) {
        self.stage = if self.recompile {
            Stage::Finished
        } else {
            Stage::Regclasses(RoutineRegclassBinding::default())
        };
    }
}

struct SearchPath<'a> {
    session: &'a dyn compilation::RoutineCompilationSession,
    previous: Option<Arc<Vec<String>>>,
}

impl Drop for SearchPath<'_> {
    fn drop(&mut self) {
        self.session.restore_shared_routine_search_path(
            self.previous.take().expect("active routine search path"),
        );
    }
}
