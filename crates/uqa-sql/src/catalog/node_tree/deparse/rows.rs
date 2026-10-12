//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct ROW constructors and selected row comparisons.

use super::{invalid, list, Field, Node, Renderer, SQLError};

impl Renderer<'_> {
    pub(super) fn is_record_argument(&self, argument: &Field) -> Result<bool, SQLError> {
        let Field::Node(node) = argument else {
            return Err(invalid("NULL test argument is not an expression"));
        };
        let field = match node.kind.as_str() {
            "ROWEXPR" => return Ok(true),
            "BOOLEXPR" | "NULLTEST" | "DISTINCTEXPR" | "SCALARARRAYOPEXPR" | "ROWCOMPAREEXPR" => {
                return Ok(false)
            }
            "COLLATEEXPR" => return self.is_record_argument(node.field("arg")?),
            "VAR" => "vartype",
            "PARAM" => "paramtype",
            "CONST" => "consttype",
            "COERCETODOMAINVALUE" | "CASETESTEXPR" => "typeId",
            "OPEXPR" | "NULLIFEXPR" => "opresulttype",
            "FUNCEXPR" => "funcresulttype",
            "RELABELTYPE" | "COERCEVIAIO" | "COERCETODOMAIN" | "ARRAYCOERCEEXPR" => "resulttype",
            "ARRAYEXPR" => "array_typeid",
            "CASEEXPR" => "casetype",
            "COALESCEEXPR" => "coalescetype",
            "MINMAXEXPR" => "minmaxtype",
            _ => return Err(invalid("unknown NULL test argument type")),
        };
        let oid = u32::try_from(node.integer(field)?)
            .map_err(|_| invalid("invalid NULL test argument type"))?;
        if i64::from(oid) == crate::catalog::type_metadata::pg_type_oid(&crate::ColumnType::Record)
        {
            return Ok(true);
        }
        if oid == 705
            || crate::catalog::type_metadata::builtin_scalar_type(oid).is_some()
            || crate::catalog::type_metadata::builtin_array_element(oid).is_some()
        {
            return Ok(false);
        }
        let ty = self
            .names
            .value_catalog()
            .ok_or_else(|| invalid("catalog type metadata is unavailable"))?
            .value_type_by_oid(oid)?
            .ok_or_else(|| invalid("unknown NULL test catalog argument type"))?;
        let mut base = &ty;
        while let crate::ColumnType::Domain { base: inner, .. } = base {
            base = inner;
        }
        Ok(matches!(
            base,
            crate::ColumnType::Record | crate::ColumnType::Composite(_)
        ))
    }

    pub(super) fn row(&self, node: &Node) -> Result<String, SQLError> {
        let arguments = list(node, "args")?;
        let names = list(node, "colnames")?;
        let record = crate::catalog::type_metadata::pg_type_oid(&crate::ColumnType::Record);
        let oid = node.integer("row_typeid")?;
        let fields = if oid == record {
            if names.len() != arguments.len()
                || names
                    .iter()
                    .any(|name| !matches!(name, Field::StringValue(_)))
            {
                return Err(invalid("invalid ROW column names"));
            }
            self.row_fields(arguments)?
        } else {
            if !names.is_empty() {
                return Err(invalid("named ROW has anonymous column names"));
            }
            let type_oid = u32::try_from(oid).map_err(|_| invalid("invalid ROW type OID"))?;
            let descriptor = self
                .names
                .value_catalog()
                .ok_or_else(|| invalid("composite type catalog is unavailable"))?
                .value_composite_type(type_oid)?
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("type with OID {type_oid} does not exist"),
                })?;
            let width = descriptor.physical_width();
            if arguments.len()
                > usize::try_from(width).map_err(|_| invalid("invalid ROW descriptor width"))?
            {
                return Err(invalid("ROW has more fields than its descriptor"));
            }
            descriptor
                .attributes
                .iter()
                .map(|attribute| {
                    let position = usize::try_from(attribute.number - 1)
                        .map_err(|_| invalid("invalid ROW attribute number"))?;
                    arguments
                        .get(position)
                        .map_or_else(|| Ok("NULL".into()), |value| self.field(value, self.pretty))
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        };
        let mut output = format!("ROW({fields})");
        match node.integer("row_format")? {
            0 | 2 => {}
            1 => {
                output.push_str("::");
                output.push_str(&self.names.type_name(oid, -1)?);
            }
            _ => return Err(invalid("invalid ROW coercion format")),
        }
        Ok(output)
    }

    pub(super) fn row_comparison(&self, node: &Node) -> Result<String, SQLError> {
        let left = list(node, "largs")?;
        let right = list(node, "rargs")?;
        let operators = oid_list(node, "opnos")?;
        let families = oid_list(node, "opfamilies")?;
        let collations = oid_list(node, "inputcollids")?;
        if left.is_empty()
            || [
                right.len(),
                operators.len(),
                families.len(),
                collations.len(),
            ]
            .into_iter()
            .any(|length| length != left.len())
            || !matches!(node.integer("cmptype")?, 1 | 2 | 4 | 5)
        {
            return Err(invalid("invalid ROW comparison operands"));
        }
        let operator = crate::type_resolution::binary_operator_by_oid(operators[0])
            .ok_or_else(|| invalid("unknown ROW comparison operator"))?;
        Ok(format!(
            "(ROW({}) {} ROW({}))",
            self.row_fields(left)?,
            operator.name,
            self.row_fields(right)?
        ))
    }

    fn row_fields(&self, arguments: &[Field]) -> Result<String, SQLError> {
        arguments
            .iter()
            .map(|argument| self.field(argument, self.pretty))
            .collect::<Result<Vec<_>, _>>()
            .map(|fields| fields.join(", "))
    }
}

fn oid_list(node: &Node, name: &str) -> Result<Vec<i64>, SQLError> {
    let [Field::Atom(tag), values @ ..] = list(node, name)? else {
        return Err(invalid("invalid ROW comparison OID list"));
    };
    if tag != "o" {
        return Err(invalid("invalid ROW comparison OID list tag"));
    }
    values
        .iter()
        .map(|value| match value {
            Field::Atom(value) => value
                .parse::<u32>()
                .map(i64::from)
                .map_err(|_| invalid("invalid ROW comparison OID")),
            _ => Err(invalid("invalid ROW comparison OID")),
        })
        .collect()
}
