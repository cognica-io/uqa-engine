//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The field schema is exhaustive over native structures, including fields omitted from durable images.

macro_rules! syntax_nodes {
    ($($variant:ident: $ty:ty),+ $(,)?) => {
        #[derive(Debug)]
        #[expect(clippy::large_enum_variant, reason = "admitted native nodes move through the existing retirement workspace without allocating boxes")]
        pub(super) enum Node { Empty, $($variant($ty),)+ }
        #[derive(Clone, Copy)]
        pub(super) enum Kind { $($variant,)+ }
        impl Kind {
            fn begin(self, memory: &mut PlanMemory) -> Result<Node, SQLError> {
                match self { $(Self::$variant => construct::<$ty>(memory),)+ }
            }
        }
        impl Node {
            fn body(&mut self) -> &mut dyn Body {
                match self { Self::Empty => unreachable!("empty syntax root"), $(Self::$variant(value) => value,)+ }
            }
        }
        $(impl Tree for $ty {
            const KIND: Kind = Kind::$variant;
            fn into_node(self) -> Node { Node::$variant(self) }
            fn from_node(node: Node) -> Self {
                let Node::$variant(value) = node else { unreachable!("native syntax constructor") };
                value
            }
        })+
    };
}

macro_rules! syntax_members {
    ($($ty:ty),+ $(,)?) => { $(
        impl Field for $ty {
            fn new(memory: &mut PlanMemory) -> Result<Self, SQLError> {
                <Self as Body>::new(memory)
            }
            fn read(&mut self, _: &mut Input<'_>, _: &mut PlanMemory) -> Result<Step, SQLError> {
                Ok(Step::Child(Self::KIND))
            }
            fn attach(&mut self, child: Node) { *self = Self::from_node(child); }
            fn retire(&mut self) -> Option<Node> {
                if <Self as Empty>::is_flat(self) { return None; }
                Some(std::mem::replace(self, <Self as Empty>::empty()).into_node())
            }
            fn needs_retirement(&self) -> bool { !<Self as Empty>::is_flat(self) }
        }
    )+ };
}

macro_rules! syntax_struct {
    ($ty:ty { $($field:ident),* $(,)? } $(skip { $($skip:ident),* $(,)? })?) => {
        impl Empty for $ty {
            fn empty() -> Self {
                Self { $($field: Empty::empty(),)* $($($skip: Empty::empty(),)*)? }
            }
            fn is_flat(&self) -> bool { true $(&& !Field::needs_retirement(&self.$field))* }
        }
        impl Body for $ty {
            fn new(_: &mut PlanMemory) -> Result<Self, SQLError> { Ok(<Self as Empty>::empty()) }
            fn field(&mut self, position: usize) -> Option<&mut dyn Field> {
                let Self { $($field,)* $($($skip: _,)*)? } = self;
                let fields = [$($field as &mut dyn Field,)*];
                fields.into_iter().nth(position)
            }
        }
    };
}

macro_rules! syntax_enum {
    ($ty:ty, $empty:expr; $($tag:literal => $variant:ident $({ $($field:ident),* $(,)? })? $(($tuple:ident))?),+ $(,)? $(reject { $($pattern:pat),+ $(,)? })?) => {
        impl Empty for $ty {
            fn empty() -> Self { $empty }
            fn is_flat(&self) -> bool {
                match self {
                    $(Self::$variant $({ $($field),* })? $(($tuple))? => {
                        true $($(&& !Field::needs_retirement($field))*)? $(&& !Field::needs_retirement($tuple))?
                    },)+
                    $($($pattern)|+ => true,)?
                }
            }
        }
        impl Body for $ty {
            fn new(_: &mut PlanMemory) -> Result<Self, SQLError> { Ok(<Self as Empty>::empty()) }
            fn first_field(&self) -> usize { 1 }
            fn field(&mut self, position: usize) -> Option<&mut dyn Field> {
                let position = position.checked_sub(1)?;
                match self {
                    $(Self::$variant $({ $($field),* })? $(($tuple))? => {
                        let fields = [$($($field as &mut dyn Field,)*)? $($tuple as &mut dyn Field,)?];
                        fields.into_iter().nth(position)
                    },)+
                    $($($pattern)|+ => None,)?
                }
            }
            fn read(&mut self, position: usize, _: &mut Option<usize>, input: &mut Input<'_>, memory: &mut PlanMemory) -> Result<Step, SQLError> {
                if position == 0 {
                    *self = match input.binary.array::<1>()?[0] {
                        $($tag => Self::$variant $({ $($field: Field::new(memory)?,)* })? $(( { let _ = stringify!($tuple); Field::new(memory)? } ))?,)+
                        _ => return Err(invalid(stringify!($ty))),
                    };
                    return Ok(Step::Advance);
                }
                self.field(position).map_or(Ok(Step::Done), |value| value.read(input, memory))
            }
        }
    };
}
