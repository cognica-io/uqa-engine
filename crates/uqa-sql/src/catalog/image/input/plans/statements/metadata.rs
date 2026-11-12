//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Body, Empty, Field, Fields, Input, PlanMemory, SQLError, Step};
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
    ($ty:ty, |$input:ident, $memory:ident, $read:ident| $body:expr) => {
        impl Field for $ty {
            fn new(_: &mut PlanMemory) -> Result<Self, SQLError> {
                Ok(<Self as Empty>::empty())
            }
            fn read(
                &mut self,
                $input: &mut Input<'_>,
                $memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                macro_rules! $read {
                    ($value:expr) => {
                        read_field!($value)
                    };
                }
                *self = $body?;
                Ok(Step::Advance)
            }
            async fn read_fields(
                &mut self,
                $input: &mut $crate::catalog::image::input::Fields<'_>,
                $memory: &mut PlanMemory,
            ) -> Result<Step, SQLError> {
                macro_rules! $read {
                    ($value:expr) => {
                        await_field!($value)
                    };
                }
                *self = $body?;
                Ok(Step::Advance)
            }
        }
    };
}
leaf!(String, |input, memory, read| read!(input.plan_text(memory)));
leaf!(bool, |input, _memory, read| read!(input.flag()));
leaf!(usize, |input, _memory, read| read!(input.binary.count()));
leaf!(Vec<String>, |input, memory, read| read!(
    input.plan_texts(memory)
));
leaf!(Vec<usize>, |input, memory, read| input_list!(
    read,
    input,
    memory,
    |input, _| { Ok(read!(input.binary.count())?) }
));
leaf!(CteMaterialization, |input, _memory, read| read!(
    input.plan_materialization()
));
leaf!(SetOpKind, |input, _memory, read| read!(
    input.plan_set_kind()
));
leaf!(JoinKind, |input, _memory, read| read!(
    input.plan_join_kind()
));
leaf!(FrameMode, |input, _memory, read| read!(
    input.plan_frame_mode()
));
leaf!(FrameExclusion, |input, _memory, read| read!(
    input.plan_exclusion()
));
leaf!(Option<NullsOrder>, |input, _memory, read| read!(
    input.plan_optional_nulls()
));
leaf!(Option<FunctionBinding>, |input, memory, read| read!(
    input.plan_binding(memory)
));
leaf!(Option<OperatorJoinRelations>, |input, memory, read| read!(
    input.plan_relations(memory)
));
leaf!(Option<JoinUsing>, |input, memory, read| read!(
    input.plan_using(memory)
));
leaf!(Option<OverridingKind>, |input, _memory, read| read!(
    input.plan_overriding()
));
leaf!(ReturningAliases, |input, memory, read| read!(
    input.plan_returning_aliases(memory)
));
leaf!(Vec<LockingClause>, |input, memory, read| read!(
    input.plan_locking(memory)
));
leaf!(Vec<Option<ColumnType>>, |input, memory, read| input_list!(
    read,
    input,
    memory,
    |input, memory| { read!(input.plan_optional_type(memory)) }
));
leaf!(Vec<Option<u32>>, |input, memory, read| input_list!(
    read,
    input,
    memory,
    |input, _| { read!(input.optional_number()) }
));
leaf!(Option<InternalRelationId>, |input, _memory, read| {
    Ok::<_, SQLError>(if read!(input.flag())? {
        Some(InternalRelationId::from_raw(u64::from_le_bytes(read!(
            input.binary.array()
        )?)))
    } else {
        None
    })
});
leaf!(Option<CteSearchClause>, |input, memory, read| {
    Ok::<_, SQLError>(if read!(input.flag())? {
        Some(CteSearchClause {
            columns: read!(input.plan_texts(memory))?,
            breadth_first: read!(input.flag())?,
            sequence_column: read!(input.plan_text(memory))?,
        })
    } else {
        None
    })
});
leaf!(Option<WindowReference>, |input, memory, read| {
    Ok::<_, SQLError>(if read!(input.flag())? {
        Some(WindowReference {
            name: read!(input.plan_text(memory))?,
            kind: match read!(input.binary.array::<1>())?[0] {
                0 => WindowReferenceKind::Direct,
                1 => WindowReferenceKind::Copy,
                _ => return Err(super::invalid("stored window reference kind")),
            },
        })
    } else {
        None
    })
});
leaf!(
    WindowCallModifiers,
    |input, _memory, read| Ok::<_, SQLError>(WindowCallModifiers {
        distinct: read!(input.flag())?,
        ordered: read!(input.flag())?,
        within_group: read!(input.flag())?,
    })
);
leaf!(uqa_core::memory::OwnedMap<String, MergeTargetColumnBinding>, |input, memory, read| read!(input.statement_columns(memory)));

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

macro_rules! statement_metadata {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            $($async)? fn statement_columns(
                &mut self,
                memory: &mut PlanMemory,
            ) -> Result<uqa_core::memory::OwnedMap<String, MergeTargetColumnBinding>, SQLError> {
                use uqa_core::memory::{OwnedMap, OwnedSet};
                let count = $read!(self.binary.count())?;
                let mut values = OwnedMap::new();
                let mut previous: Option<(String, MergeTargetColumnBinding)> = None;
                for _ in 0..count {
                    $read!(self.checkpoint())?;
                    memory.grow(OwnedMap::<String, MergeTargetColumnBinding>::entry_bytes())?;
                    let name = $read!(self.plan_text(memory))?;
                    if let Some((prior, _)) = &previous {
                        if $read!(self.compare_text(prior, &name))? != std::cmp::Ordering::Less {
                            return Err(super::invalid("stored MERGE column order"));
                        }
                    }
                    let object_id = $read!(self.binary.array())?;
                    let attribute_number = if $read!(self.flag())? {
                        Some(i16::from_le_bytes($read!(self.binary.array())?))
                    } else {
                        None
                    };
                    let mut domain_dependencies = OwnedSet::new();
                    let mut prior_oid = None;
                    for _ in 0..$read!(self.binary.count())? {
                        $read!(self.checkpoint())?;
                        let oid = u32::from_le_bytes($read!(self.binary.array())?);
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
    };
}
statement_metadata!(Input, read_field);
statement_metadata!(Fields, await_field, async);
