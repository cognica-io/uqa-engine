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
            compile_function_body, defer_function_body, with_routine_parser_settings,
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
        let scope = self.scope(def.declaration());
        scope.run(context, || {
            self.advance_scoped(context, def, control, statement)
        })
    }

    fn scope(&self, def: &CreateFunction) -> BindingScope {
        let stored = self.bodies == RoutineBodyCompilation::Stored;
        let standard = matches!(def.body, FunctionBody::Statements(_));
        let needs_path = match self.stage {
            Stage::Regclasses(_) => true,
            Stage::Dependencies(_) => !self.search_path.is_empty(),
            Stage::Compile | Stage::Statements(_) => {
                stored && standard && !self.search_path.is_empty()
            }
            Stage::Finished => false,
        };
        BindingScope {
            stored,
            search_path: needs_path.then(|| Arc::clone(&self.search_path)),
            parser: matches!(self.stage, Stage::Statements(_))
                || matches!(self.stage, Stage::Compile) && standard,
        }
    }

    /// The deferred operation enters its defining namespace anew for each poll.
    pub(super) fn retained_scope(&self, def: &CreateFunction) -> Option<BindingScope> {
        (matches!(self.stage, Stage::Compile) && matches!(def.body, FunctionBody::Statements(_)))
            .then(|| self.scope(def))
    }

    pub(super) async fn advance_input<R: uqa_sql::routines::compilation::StoredRoutineCatalog>(
        &mut self,
        catalog: &R,
        def: &mut StoredRoutineDefinition,
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        let Stage::Compile = self.stage else {
            unreachable!("selected stored compilation");
        };
        self.stage = Stage::Statements(
            SQLStandardBodyCompilation::new_from_input(catalog, def.declaration(), control, input)
                .await?,
        );
        Ok(())
    }

    fn advance_scoped<D: RoutineDeclaration>(
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
        let analysis = &context.compilation.analysis;
        let stored = self.bodies == RoutineBodyCompilation::Stored;
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
                    self.stage = Stage::Statements(SQLStandardBodyCompilation::new(
                        analysis, def, stored, control,
                    )?);
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
            Stage::Statements(input) => match statement(input, def, analysis)? {
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
            },
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

pub(super) struct BindingScope {
    stored: bool,
    search_path: Option<Arc<Vec<String>>>,
    parser: bool,
}

impl BindingScope {
    pub(super) fn run<T>(
        &self,
        context: &RoutineDefinitionContext<'_>,
        operation: impl FnOnce() -> T,
    ) -> T {
        let _placement = self.stored.then(SetPlacementScope::stored_definition);
        let session = context.compilation.session;
        let _path = self.search_path.as_ref().map(|path| SearchPath {
            session,
            previous: Some(session.replace_shared_routine_search_path(Arc::clone(path))),
        });
        if self.parser {
            with_routine_parser_settings(context.compilation.analysis.parsers, operation)
        } else {
            operation()
        }
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
