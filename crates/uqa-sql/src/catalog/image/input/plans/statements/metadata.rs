//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Body, Empty, Field, Input, PlanMemory, SQLError, Step};
use crate::ast::{
    ColumnType, CteMaterialization, CteSearchClause, FrameExclusion, FrameMode, FunctionBinding,
    InternalRelationId, JoinKind, JoinUsing, LockingClause, MergeTargetColumnBinding, NullsOrder,
    OperatorJoinRelations, OrderBy, OverridingKind, Projection, ReturningAliases, SetOpKind,
    TableFunction, WindowCallModifiers, WindowDefinition, WindowFrame, WindowReference,
    WindowReferenceKind, WindowSpec,
};

macro_rules! empty {
    ($($ty:ty => $value:expr),+ $(,)?) => { $(
        impl Empty for $ty { fn empty() -> Self { $value } }
    )+ };
}
empty! {
    String => String::new(), bool => false, usize => 0,
    CteMaterialization => CteMaterialization::Default, SetOpKind => SetOpKind::Union,
    JoinKind => JoinKind::Inner, FrameMode => FrameMode::Rows,
    FrameExclusion => FrameExclusion::NoOthers,
    ReturningAliases => ReturningAliases { old: String::new(), new: String::new(), old_explicit: false, new_explicit: false },
    WindowCallModifiers => WindowCallModifiers::default(),
    uqa_core::memory::OwnedMap<String, MergeTargetColumnBinding> => uqa_core::memory::OwnedMap::new(),
}

macro_rules! leaf {
    ($ty:ty, |$input:ident, $memory:ident| $read:expr) => {
        impl Field for $ty {
            fn new(_: &mut PlanMemory) -> Result<Self, SQLError> {
                Ok(<Self as Empty>::empty())
            }
            fn read(
                &mut self,
                $input: &mut Input<'_>,
                $memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                *self = $read?;
                Ok(Step::Advance)
            }
        }
    };
}
leaf!(String, |input, memory| input.plan_text(memory));
leaf!(bool, |input, _memory| input.flag());
leaf!(usize, |input, _memory| input.binary.count());
leaf!(Vec<String>, |input, memory| input.plan_texts(memory));
leaf!(Vec<usize>, |input, memory| input
    .plan_vector(memory, |input, _| Ok(input.binary.count()?)));
leaf!(CteMaterialization, |input, _memory| input
    .plan_materialization());
leaf!(SetOpKind, |input, _memory| input.plan_set_kind());
leaf!(JoinKind, |input, _memory| input.plan_join_kind());
leaf!(FrameMode, |input, _memory| input.plan_frame_mode());
leaf!(FrameExclusion, |input, _memory| input.plan_exclusion());
leaf!(Option<NullsOrder>, |input, _memory| input
    .plan_optional_nulls());
leaf!(Option<FunctionBinding>, |input, memory| input
    .plan_binding(memory));
leaf!(Option<OperatorJoinRelations>, |input, memory| input
    .plan_relations(memory));
leaf!(Option<JoinUsing>, |input, memory| input.plan_using(memory));
leaf!(Option<OverridingKind>, |input, _memory| input
    .plan_overriding());
leaf!(ReturningAliases, |input, memory| input
    .plan_returning_aliases(memory));
leaf!(Vec<LockingClause>, |input, memory| input
    .plan_locking(memory));
leaf!(Vec<Option<ColumnType>>, |input, memory| input
    .plan_vector(memory, Input::plan_optional_type));
leaf!(Vec<Option<u32>>, |input, memory| input
    .plan_vector(memory, |input, _| input.optional_number()));
leaf!(Option<InternalRelationId>, |input, _memory| input
    .flag()?
    .then(|| input
        .binary
        .array()
        .map(|bytes| InternalRelationId::from_raw(u64::from_le_bytes(bytes))))
    .transpose());
leaf!(Option<CteSearchClause>, |input, memory| input
    .flag()?
    .then(|| Ok::<_, SQLError>(CteSearchClause {
        columns: input.plan_texts(memory)?,
        breadth_first: input.flag()?,
        sequence_column: input.plan_text(memory)?,
    }))
    .transpose());
leaf!(Option<WindowReference>, |input, memory| input
    .flag()?
    .then(|| Ok::<_, SQLError>(WindowReference {
        name: input.plan_text(memory)?,
        kind: match input.binary.array::<1>()?[0] {
            0 => WindowReferenceKind::Direct,
            1 => WindowReferenceKind::Copy,
            _ => return Err(super::invalid("stored window reference kind")),
        },
    }))
    .transpose());
leaf!(WindowCallModifiers, |input, _memory| Ok::<_, SQLError>(
    WindowCallModifiers {
        distinct: input.flag()?,
        ordered: input.flag()?,
        within_group: input.flag()?,
    }
));
leaf!(uqa_core::memory::OwnedMap<String, MergeTargetColumnBinding>, |input, memory| input.statement_columns(memory));

syntax_struct!(Projection { expr, alias });
syntax_struct!(OrderBy {
    expr,
    descending,
    nulls
});
syntax_struct!(TableFunction {
    name,
    binding,
    output_name,
    relations,
    args,
    column_aliases,
    column_types
});
syntax_struct!(WindowDefinition {
    name,
    inherited,
    spec
});
syntax_struct!(WindowSpec { definition, reference, partition_by, order_by, frame } skip { raw_definition });
syntax_struct!(WindowFrame {
    mode,
    start,
    end,
    between,
    exclusion
});

impl Input<'_> {
    fn statement_columns(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<uqa_core::memory::OwnedMap<String, MergeTargetColumnBinding>, SQLError> {
        use uqa_core::memory::{OwnedMap, OwnedSet};
        let count = self.binary.count()?;
        let mut values = OwnedMap::new();
        let mut previous: Option<(String, MergeTargetColumnBinding)> = None;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            memory.grow(OwnedMap::<String, MergeTargetColumnBinding>::entry_bytes())?;
            let name = self.plan_text(memory)?;
            if let Some((prior, _)) = &previous {
                if self.compare_text(prior, &name)? != std::cmp::Ordering::Less {
                    return Err(super::invalid("stored MERGE column order"));
                }
            }
            let object_id = self.binary.array()?;
            let attribute_number = self
                .flag()?
                .then(|| self.binary.array().map(i16::from_le_bytes))
                .transpose()?;
            let mut domain_dependencies = OwnedSet::new();
            let mut prior_oid = None;
            for _ in 0..self.binary.count()? {
                self.binary.cancellation.check()?;
                let oid = u32::from_le_bytes(self.binary.array()?);
                if prior_oid.is_some_and(|previous| previous >= oid) {
                    return Err(super::invalid("stored MERGE domain order"));
                }
                memory.grow(OwnedSet::<u32>::entry_bytes())?;
                domain_dependencies.insert(oid);
                prior_oid = Some(oid);
            }
            let binding = MergeTargetColumnBinding {
                object_id,
                attribute_number,
                domain_dependencies,
            };
            if let Some((key, value)) = previous.replace((name, binding)) {
                values.insert(key, value);
            }
        }
        if let Some((key, value)) = previous {
            values.insert(key, value);
        }
        Ok(values)
    }
}
