//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Body, Empty, Field, Input, Node, PlanMemory, SQLError, Step, Tree};
use crate::ast::{CteBody, CteCycleClause, SelectStmt, SetOp, Statement, CTE};

syntax_struct!(SelectStmt {
    windows,
    projections,
    values,
    from,
    r#where,
    group_by,
    grouping_sets,
    group_distinct,
    having,
    order_by,
    limit,
    with_ties,
    offset,
    with,
    set_op,
    distinct,
    distinct_on,
    locking,
});
syntax_struct!(SetOp {
    kind,
    all,
    left,
    right,
    combined_order_by,
    combined_limit,
    combined_with_ties,
    combined_offset
});
syntax_struct!(CteCycleClause {
    columns,
    mark_column,
    mark_value,
    mark_default,
    path_column
});

syntax_enum!(Statement, Statement::Empty;
    0 => Select(value), 1 => Values { rows }, 2 => Insert(value), 3 => Update(value),
    4 => Delete(value), 5 => Merge(value), 6 => Notify { channel, payload },
    reject { _ }
);

impl Body for CTE {
    fn new(memory: &mut PlanMemory) -> Result<Self, SQLError> {
        Ok(Self {
            name: String::new(),
            columns: Vec::new(),
            recursive: false,
            materialization: crate::ast::CteMaterialization::Default,
            search: None,
            cycle: None,
            body: Field::new(memory)?,
        })
    }
    fn field(&mut self, position: usize) -> Option<&mut dyn Field> {
        let Self {
            name,
            columns,
            recursive,
            materialization,
            search,
            cycle,
            body,
        } = self;
        let fields: [&mut dyn Field; 7] = [
            name,
            columns,
            recursive,
            materialization,
            search,
            cycle,
            body,
        ];
        fields.into_iter().nth(position)
    }
}

impl Field for CteBody {
    fn new(memory: &mut PlanMemory) -> Result<Self, SQLError> {
        Ok(Self::Query(Field::new(memory)?))
    }
    fn read(&mut self, input: &mut Input<'_>, memory: &mut PlanMemory) -> Result<Step, SQLError> {
        use crate::ast::{DeleteStmt, InsertStmt, MergeStmt, UpdateStmt};
        let kind = match input.binary.array::<1>()?[0] {
            0 => return Ok(Step::Child(SelectStmt::KIND)),
            1 => {
                *self = Self::Insert(Field::new(memory)?);
                InsertStmt::KIND
            }
            2 => {
                *self = Self::Update(Field::new(memory)?);
                UpdateStmt::KIND
            }
            3 => {
                *self = Self::Delete(Field::new(memory)?);
                DeleteStmt::KIND
            }
            4 => {
                *self = Self::Merge(Field::new(memory)?);
                MergeStmt::KIND
            }
            _ => return Err(super::invalid("stored statement CTE body")),
        };
        drop(memory.split(size_of::<SelectStmt>()));
        Ok(Step::Child(kind))
    }
    fn attach(&mut self, child: Node) {
        match self {
            Self::Query(value) => Field::attach(value, child),
            Self::Insert(value) => Field::attach(value, child),
            Self::Update(value) => Field::attach(value, child),
            Self::Delete(value) => Field::attach(value, child),
            Self::Merge(value) => Field::attach(value, child),
        }
    }
    fn retire(&mut self) -> Option<Node> {
        match self {
            Self::Query(value) => Field::retire(value),
            Self::Insert(value) => Field::retire(value),
            Self::Update(value) => Field::retire(value),
            Self::Delete(value) => Field::retire(value),
            Self::Merge(value) => Field::retire(value),
        }
    }
}
