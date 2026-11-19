//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rebind a stored parameter in its private field before publishing the edited declaration.

use super::{RoutineCompilationMode, RoutineDeclaration};
use crate::plan::copy::stored_plan::BindingSyntax;
use crate::plan::copy::stored_plan::DefaultExpression;
use crate::{
    ast::{Expr, FunctionParam, RoutineDefaultType, UserTypeIdentity},
    binding::{
        bind_expression_plan_routines_for_storage,
        bind_expression_plan_routines_for_storage_with_work,
        syntax_sites::{expression_sites_from, SiteInput},
    },
    catalog::{
        stored_ast,
        work::{compare_text, ReferenceWork, Work},
    },
    plan::ExpressionPlan,
    routines::{
        compilation::{RoutineCompilationContext, StoredRoutineCatalog},
        declaration::RoutineTypeCatalog,
        AggregateLookup, RoutineResolution, StoredRoutineDefinition,
    },
    type_resolution::routine_polymorphic_type_with_work,
    ColumnType, RowSchema, SQLError,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
};

/// The ordinary declaration supplies its callback catalog while a restored definition supplies the private session. Both apply the same type annotation and syntax changes in declaration order.
struct DefaultCatalog<'a, T: ?Sized> {
    types: &'a T,
    routines: &'a dyn RoutineResolution,
}

struct BoundDefault {
    original: DefaultExpression,
    bound: DefaultExpression,
    source: Option<Produced<ColumnType>>,
}

pub(super) fn bind_default(
    context: &RoutineCompilationContext<'_>,
    parameter: &mut FunctionParam,
    mode: RoutineCompilationMode,
) -> Result<bool, SQLError> {
    let Some(default) = parameter.default.as_ref() else {
        return Ok(false);
    };
    let control = ProductionControl::uncontrolled();
    let original = ExpressionPlan::lower_with(default.clone(), &|name: &str| {
        context.catalog.has_registered_aggregate_function(name)
    });
    let mut bound = original.clone();
    let snapshot = context.catalog.binding_snapshot()?;
    let ty = bind_expression_plan_routines_for_storage(
        context.routines,
        &mut bound,
        &[],
        &snapshot.context(),
        &RowSchema::default(),
    )?;
    let plans = BoundDefault {
        original: DefaultExpression::new(control.finish(original, None)?, None),
        bound: DefaultExpression::new(control.finish(bound, None)?, None),
        source: ty.map(|ty| control.finish(ty, None)).transpose()?,
    };
    let catalog = DefaultCatalog {
        types: context.types,
        routines: context.routines,
    };
    Ok(
        crate::catalog::work::inline(plans.apply(parameter, catalog, mode, &control, &mut ()))?
            .into_uncontrolled()
            .expect("ordinary default"),
    )
}

pub(crate) async fn bind_stored_parameter_default_from(
    catalog: &impl StoredRoutineCatalog,
    definition: &mut StoredRoutineDefinition,
    position: usize,
    mode: RoutineCompilationMode,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Option<bool>, SQLError> {
    let Some(parameter) = definition.declaration().params.get(position) else {
        return Ok(None);
    };
    if parameter.default.is_none() {
        return Ok(Some(false));
    }
    let mut edit = definition
        .parameter_edit_from(position, control, input)
        .await?
        .expect("selected stored parameter");
    let parameter = edit.value_mut();
    let default = parameter.default.as_ref().expect("selected stored default");
    let (lowered, workspace) = ExpressionPlan::lower_borrowed_with_work(
        default,
        &AggregateLookup(catalog),
        control,
        input,
    )
    .await?;
    let original = lowered
        .capture_syntax_before_binding(control, input)
        .await?;
    let original = DefaultExpression::new(original, workspace.as_ref());
    let snapshot = catalog.binding_snapshot_with_control(control)?;
    let (bound, source) = bind_expression_plan_routines_for_storage_with_work(
        catalog,
        (lowered, workspace.as_ref()),
        &[],
        snapshot.context_with_control(control)?,
        &RowSchema::default(),
        control,
        input,
    )
    .await?;
    let bound = DefaultExpression::new(bound, workspace.as_ref());
    drop(workspace);
    let catalog = DefaultCatalog {
        types: catalog,
        routines: catalog,
    };
    let applied = BoundDefault {
        original,
        bound,
        source,
    }
    .apply(parameter, catalog, mode, control, input)
    .await?;
    let changed = edit.finish_from(*applied, control, input).await?;
    drop(applied);
    Ok(Some(changed))
}

impl BoundDefault {
    async fn apply<T: RoutineTypeCatalog + ?Sized, W: Work + SiteInput>(
        self,
        parameter: &mut FunctionParam,
        catalog: DefaultCatalog<'_, T>,
        mode: RoutineCompilationMode,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<bool>, SQLError>
    where
        W::Error: Into<SQLError> + Send,
    {
        let FunctionParam {
            default,
            type_name,
            default_type,
            ..
        } = parameter;
        let default = default.as_mut().expect("selected parameter default");
        let Self {
            original,
            bound,
            source,
        } = self;
        let selected = async {
            let source =
                annotation(catalog.types, type_name, default, source, control, work).await?;
            let annotation = replace_annotation(default_type, source, control, work).await?;
            let sites = expression_sites_from(&original, &bound, control, work).await?;
            Ok::<_, SQLError>((sites, annotation))
        }
        .await;
        original.retire(control, work).await?;
        bound.retire(control, work).await?;
        let (sites, changed) = selected?;
        let applied = mode
            .bind_expression_sites_with_work(default, &sites, control, work)
            .await?;
        let (sites, lease) = sites.into_parts();
        crate::catalog::work::retire_entries(sites.routines.into_iter(), control, work).await?;
        crate::catalog::work::retire_entries(sites.values.into_iter(), control, work).await?;
        drop(lease);
        let (annotation_changed, annotation_lease) = changed.into_parts();
        let (sites_changed, sites_lease) = applied.into_parts();
        let mut changed = annotation_changed || sites_changed;
        let mut memory = control.combine(annotation_lease, sites_lease);
        if UserTypeIdentity::parse_with_type_work(
            type_name,
            control,
            &mut ReferenceWork { control, work },
        )
        .await?
        .is_some()
        {
            if let Some(ty) = catalog
                .routines
                .resolve_type_name_with_control(type_name, control)?
            {
                let (folded, literals) = stored_ast::fold_assigned_stored_literal_from(
                    default,
                    &ty,
                    &|| catalog.routines.enum_labels(),
                    control,
                    work,
                )
                .await?
                .into_parts();
                changed |= folded;
                memory = control.combine(memory, literals);
            }
        }
        Ok(control.finish(changed, memory)?)
    }
}

async fn replace_annotation(
    current: &mut Option<RoutineDefaultType>,
    source: Option<Produced<RoutineDefaultType>>,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<Produced<bool>, SQLError> {
    let changed = !same_annotation(current.as_ref(), source.as_deref(), control, work).await?;
    let (value, memory) = source.map_or((None, None), |source| {
        let (value, memory) = source.into_parts();
        (Some(value), memory)
    });
    let discarded = if changed {
        std::mem::replace(current, value)
    } else {
        value
    };
    // The field owns its existing lease until reprojection. Retire the discarded descriptor before transferring or releasing the inferred type's lease.
    if let Some(RoutineDefaultType::Concrete(ty)) = discarded {
        ColumnType::retire_with_work(
            control.finish(ty, control.empty_reservation())?,
            control,
            &mut ReferenceWork { control, work },
        )
        .await?;
    }
    Ok(control.finish(
        changed,
        if changed {
            memory
        } else {
            control.empty_reservation()
        },
    )?)
}

async fn annotation(
    catalog: &(impl RoutineTypeCatalog + ?Sized),
    type_name: &str,
    expression: &Expr,
    source: Option<Produced<ColumnType>>,
    control: &ProductionControl<'_>,
    input: &mut (impl Work + Send),
) -> Result<Option<Produced<RoutineDefaultType>>, SQLError> {
    let polymorphic = routine_polymorphic_type_with_work(type_name, control, input).await?;
    let source = if polymorphic.is_some() {
        source
    } else {
        let mut work = ReferenceWork {
            control,
            work: input,
        };
        if let Some(source) = source {
            ColumnType::retire_with_work(source, control, &mut work).await?;
        }
        let declared = catalog.resolve_catalog_column_type_name_with_control(type_name, control)?;
        Some(
            declared
                .without_modifiers_with_work(control, &mut work)
                .await?,
        )
    };
    Ok(match source {
        Some(ty) => {
            let (ty, memory) = ty.into_parts();
            Some(control.finish(RoutineDefaultType::Concrete(ty), memory)?)
        }
        None if polymorphic.is_some_and(|ty| {
            crate::routines::defaults::null_default_retains_declared_type(expression, ty)
        }) =>
        {
            let (name, memory) = Work::text(input, type_name, control).await?.into_parts();
            Some(control.finish(RoutineDefaultType::Polymorphic(name), memory)?)
        }
        None => None,
    })
}

async fn same_annotation(
    existing: Option<&RoutineDefaultType>,
    selected: Option<&RoutineDefaultType>,
    control: &ProductionControl<'_>,
    input: &mut (impl Work + Send),
) -> Result<bool, SQLError> {
    match (existing, selected) {
        (None, None) => Ok(true),
        (
            Some(RoutineDefaultType::Polymorphic(left)),
            Some(RoutineDefaultType::Polymorphic(right)),
        ) => Ok(compare_text(input, left, right, control).await?.is_eq()),
        (Some(RoutineDefaultType::Concrete(left)), Some(RoutineDefaultType::Concrete(right))) => {
            crate::type_resolution::same_declaration_with_work(
                left,
                right,
                control,
                &mut ReferenceWork {
                    control,
                    work: input,
                },
            )
            .await
        }
        _ => Ok(false),
    }
}
