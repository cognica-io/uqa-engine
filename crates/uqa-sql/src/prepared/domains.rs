//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain coercions in a generic executable retain the assumptions under which their checks could be folded away.

use super::ownership::SharedPreparedInput;
use crate::catalog::domain::{coercion_constraints, CoercionConstraints};
use crate::{
    ast::{UserTypeIdentity, UserTypeKind},
    plan::UnifiedPlan,
    routines::declaration::RoutineTypeCatalog,
    ColumnType, SQLError, ScalarExpr,
};
use uqa_core::memory::{ProductionControl, ProductionVec};

#[derive(Debug)]
struct DomainBinding {
    name: String,
    oid: u32,
    unconstrained: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GenericDomainInputs(Option<SharedPreparedInput<Vec<DomainBinding>>>);

impl GenericDomainInputs {
    /// Bind written cast targets before optimizing. A domain chain that still had a check keeps its runtime coercion when a check is removed.
    pub fn capture(
        plan: &UnifiedPlan,
        catalog: &dyn RoutineTypeCatalog,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut bindings = ProductionVec::<DomainBinding>::new(*control);
        let mut visit = |expr: &ScalarExpr| -> Result<(), SQLError> {
            expr.try_visit(&mut |node| {
                control.check()?;
                let ScalarExpr::Cast { ty: name, .. } = node else {
                    return Ok(true);
                };
                // Grammar-bound builtins have a fixed catalog identity. A free
                // type name can still select a domain through the search path.
                if bindings.iter().any(|binding| binding.name == *name)
                    || crate::type_resolution::pg_catalog_builtin_type_with_control(name, control)?
                        .is_some()
                {
                    return Ok(true);
                }
                let Some(resolved) =
                    catalog.resolve_catalog_column_type_with_control(name, control)?
                else {
                    return Ok(true);
                };
                let ColumnType::Domain { oid, .. } = &*resolved else {
                    return Ok(true);
                };
                let unconstrained = match coercion_constraints(&resolved, catalog, control)? {
                    CoercionConstraints::Unconstrained(_) => true,
                    CoercionConstraints::Checked => false,
                    CoercionConstraints::Unknown => return Ok(true),
                };
                let (name, memory) = control.copy_text(name)?.into_parts();
                bindings.push_produced(control.finish(
                    DomainBinding {
                        name,
                        oid: *oid,
                        unconstrained,
                    },
                    memory,
                )?)?;
                Ok(true)
            })
        };
        match plan {
            UnifiedPlan::Query(query) => query.try_visit_scalar_expressions(&mut visit)?,
            UnifiedPlan::Command(cmd) => cmd.try_visit_scalar_expressions(&mut visit)?,
        }
        if bindings.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self(Some(SharedPreparedInput::from_produced(
            bindings.finish()?,
            control,
        )?)))
    }

    pub fn requires_rebuild(
        &self,
        catalog: &dyn RoutineTypeCatalog,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let Some(bindings) = &self.0 else {
            return Ok(false);
        };
        for binding in bindings.iter() {
            control.check()?;
            let current =
                catalog.resolve_catalog_column_type_with_control(&binding.name, control)?;
            let Some(ty @ ColumnType::Domain { oid, .. }) = current.as_deref() else {
                return Ok(true);
            };
            if *oid != binding.oid
                || (binding.unconstrained
                    && !matches!(
                        coercion_constraints(ty, catalog, control)?,
                        CoercionConstraints::Unconstrained(_)
                    ))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn tracks_oid(&self, oid: u32) -> bool {
        self.0
            .as_ref()
            .is_some_and(|bindings| bindings.iter().any(|binding| binding.oid == oid))
    }

    pub(super) fn tracks_name(&self, name: &str) -> bool {
        self.0
            .as_ref()
            .is_some_and(|bindings| bindings.iter().any(|binding| binding.name == name))
            || UserTypeIdentity::parse(name)
                .is_some_and(|ty| ty.kind == UserTypeKind::Domain && self.tracks_oid(ty.oid))
    }
}
