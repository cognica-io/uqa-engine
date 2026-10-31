//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

mod acl_role_specification;
mod assignment_target;
mod composites;
mod constraints;
mod cte;
mod domains;
mod enum_functions;
mod enums;
mod events;
mod expressions;
mod foreign_wrappers;
mod from;
mod function_binding;
mod identity_sequence;
mod indexes;
mod interval;
mod locking;
mod namespaces;
mod overriding;
mod pattern_operator;
mod ranges;
mod referenced_partition;
mod relation_hierarchy;
mod relation_lifecycle;
mod role_specification;
mod routine_attributes;
mod routine_security;
mod routines;
mod sequence;
mod sequence_declaration;
mod statements;
mod table_alteration;
mod type_lifecycle;
mod type_privileges;
