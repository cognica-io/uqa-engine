//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable selected types can be shared by ordered observations without copying their names or nested declarations.

use crate::{ColumnType, SQLError};
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Clone, Debug)]
pub(in crate::binding::preparation) enum AnalysisType {
    Inline(ColumnType),
    Shared(Arc<Produced<ColumnType>>),
}

impl AnalysisType {
    pub(in crate::binding::preparation) fn array(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let element = self.into_produced(control)?;
        let container = control.reserve(size_of::<ColumnType>())?;
        let (element, memory) = element.into_parts();
        Self::from_produced(
            control.finish(
                ColumnType::Array(Box::new(element)),
                control.combine(memory, container),
            )?,
            control,
        )
    }
    pub(in crate::binding::preparation) fn from_produced(
        ty: Produced<ColumnType>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        if matches!(
            &*ty,
            ColumnType::Named(_)
                | ColumnType::Array(_)
                | ColumnType::Domain { .. }
                | ColumnType::Enum(_)
                | ColumnType::Composite(_)
                | ColumnType::DeclaredRange(_)
                | ColumnType::DeclaredMultirange(_)
        ) {
            Ok(Self::Shared(ty.into_shared(control)?))
        } else {
            assert_eq!(
                ty.reserved_bytes(),
                0,
                "inline SQL type has no payload allocation"
            );
            Ok(Self::Inline(ty.into_parts().0))
        }
    }

    pub(in crate::binding::preparation) fn ordinary(ty: ColumnType) -> Self {
        let control = ProductionControl::uncontrolled();
        Self::from_produced(
            control.finish(ty, None).expect("ordinary selected type"),
            &control,
        )
        .expect("ordinary selected type owner")
    }

    pub(in crate::binding::preparation) fn into_produced(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, SQLError> {
        crate::catalog::work::inline(self.into_produced_from(control, &mut ())).map_err(Into::into)
    }

    pub(in crate::binding::preparation) async fn into_produced_from<
        W: crate::plan::ScalarInputWork,
    >(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<ColumnType>, W::Error> {
        match self {
            Self::Inline(ty) => Ok(control.finish(ty, control.empty_reservation())?),
            Self::Shared(ty) => {
                let output = work.ty(&ty, control).await?;
                Self::Shared(ty).retire_from(control, work).await?;
                Ok(output)
            }
        }
    }

    pub(in crate::binding::preparation) async fn retire_from<W: crate::plan::ScalarInputWork>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), W::Error> {
        if let Self::Shared(ty) = self {
            if let Some(ty) = Arc::into_inner(ty) {
                // Tuple fields retire in order: the selected type precedes its original payload and shared-header reservation on every exit.
                let mut owned = ty.into_parts();
                loop {
                    control.check()?;
                    work.checkpoint().await?;
                    control.check()?;
                    if owned.0.retire_step() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    pub(in crate::binding::preparation) fn into_ordinary(self) -> ColumnType {
        if let Self::Shared(ty) = &self {
            assert_eq!(
                ty.reserved_bytes(),
                0,
                "controlled type observation requires its owned result"
            );
        }
        self.into_produced(&ProductionControl::uncontrolled())
            .expect("ordinary type observation")
            .into_uncontrolled()
            .expect("ordinary result type")
    }
}

impl std::ops::Deref for AnalysisType {
    type Target = ColumnType;
    fn deref(&self) -> &ColumnType {
        match self {
            Self::Inline(ty) => ty,
            Self::Shared(ty) => ty,
        }
    }
}
impl PartialEq for AnalysisType {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl Eq for AnalysisType {}
impl PartialEq<ColumnType> for AnalysisType {
    fn eq(&self, other: &ColumnType) -> bool {
        **self == *other
    }
}
