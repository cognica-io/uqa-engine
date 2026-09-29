//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `aclexplode(aclitem[])`: one row per granted privilege of each ACL item, in item order and then in `AclMode` bit order, as `aclexplode` reports them.

use uqa_core::Value;
use uqa_sql::SQLError;

use super::TableFunctionRows;

/// Privilege letters in `AclMode` bit order with the names `convert_aclright_to_string` reports.
const PRIVILEGES: &[(char, &str)] = &[
    ('a', "INSERT"),
    ('r', "SELECT"),
    ('w', "UPDATE"),
    ('d', "DELETE"),
    ('D', "TRUNCATE"),
    ('x', "REFERENCES"),
    ('t', "TRIGGER"),
    ('X', "EXECUTE"),
    ('U', "USAGE"),
    ('C', "CREATE"),
    ('T', "TEMPORARY"),
    ('c', "CONNECT"),
    ('s', "SET"),
    ('A', "ALTER SYSTEM"),
    ('m', "MAINTAIN"),
];

pub fn aclexplode_row_stream(
    role_oid: &dyn Fn(&str) -> Result<Option<i64>, SQLError>,
    evaluated: Vec<Value>,
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    let [acl] = evaluated.as_slice() else {
        return Err(SQLError::BadArity {
            name: "aclexplode".into(),
            expected: "1".into(),
            actual: evaluated.len(),
        });
    };
    let items = match acl {
        Value::Null => Vec::new(),
        Value::Array(array) => array.elements().to_vec(),
        Value::List(values) => values.clone(),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "aclexplode requires aclitem[], got {other:?}"
            )))
        }
    };
    let mut rows = Vec::new();
    for item in items {
        let Value::Str(text) = item else {
            return Err(SQLError::Internal(format!(
                "aclitem array holds a non-text item {item:?}"
            )));
        };
        let parsed = AclItemText::parse(&text)?;
        let grantee = match parsed.grantee.as_deref() {
            None => 0,
            Some(name) => role_oid(name)?.ok_or_else(|| missing_role(name))?,
        };
        let grantor = role_oid(&parsed.grantor)?.ok_or_else(|| missing_role(&parsed.grantor))?;
        for (letter, name) in PRIVILEGES {
            let Some(grantable) = parsed
                .privileges
                .iter()
                .find_map(|(privilege, grantable)| (privilege == letter).then_some(*grantable))
            else {
                continue;
            };
            rows.push(crate::PhysicalRow::from_values(vec![
                Value::Int(grantor),
                Value::Int(grantee),
                Value::Str((*name).into()),
                Value::Bool(grantable),
            ]));
        }
    }
    let columns = ["grantor", "grantee", "privilege_type", "is_grantable"]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            column_aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| (*name).to_string())
        })
        .collect();
    Ok(TableFunctionRows::new(
        columns,
        Box::new(rows.into_iter().map(Ok)),
    ))
}

fn missing_role(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("role \"{name}\" does not exist"),
    }
}

/// The text form `grantee=privileges/grantor` of an ACL item; an empty grantee is PUBLIC and `*` marks a grantable privilege.
struct AclItemText {
    grantee: Option<String>,
    privileges: Vec<(char, bool)>,
    grantor: String,
}

impl AclItemText {
    fn parse(text: &str) -> Result<Self, SQLError> {
        let invalid = || SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("invalid input syntax for type aclitem: \"{text}\""),
        };
        let (grantee, rest) = identifier(text).ok_or_else(invalid)?;
        let rest = rest.strip_prefix('=').ok_or_else(invalid)?;
        let (privileges, grantor) = rest.split_once('/').ok_or_else(invalid)?;
        let mut parsed = Vec::new();
        let mut letters = privileges.chars().peekable();
        while let Some(letter) = letters.next() {
            if !PRIVILEGES.iter().any(|(candidate, _)| *candidate == letter) {
                return Err(invalid());
            }
            let grantable = letters.next_if_eq(&'*').is_some();
            parsed.push((letter, grantable));
        }
        let (grantor, rest) = identifier(grantor).ok_or_else(invalid)?;
        if !rest.is_empty() || grantor.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            grantee: (!grantee.is_empty()).then_some(grantee),
            privileges: parsed,
            grantor,
        })
    }
}

/// One role name of an ACL item: quoted with doubled quotes, or bare up to `=` or `/`.
fn identifier(text: &str) -> Option<(String, &str)> {
    let Some(quoted) = text.strip_prefix('"') else {
        let end = text.find(['=', '/']).unwrap_or(text.len());
        return Some((text[..end].to_string(), &text[end..]));
    };
    let mut name = String::new();
    let mut characters = quoted.char_indices();
    while let Some((index, character)) = characters.next() {
        if character != '"' {
            name.push(character);
            continue;
        }
        if quoted[index + 1..].starts_with('"') {
            name.push('"');
            characters.next();
            continue;
        }
        return Some((name, &quoted[index + 1..]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::AclItemText;

    #[test]
    fn acl_items_parse_public_quoted_and_grantable_entries() {
        let public = AclItemText::parse("=U/tl_owner").unwrap();
        assert_eq!(public.grantee, None);
        assert_eq!(public.privileges, vec![('U', false)]);
        assert_eq!(public.grantor, "tl_owner");
        let quoted = AclItemText::parse("\"Odd\"\"Role\"=U*r/\"Grant Or\"").unwrap();
        assert_eq!(quoted.grantee.as_deref(), Some("Odd\"Role"));
        assert_eq!(quoted.privileges, vec![('U', true), ('r', false)]);
        assert_eq!(quoted.grantor, "Grant Or");
        assert!(AclItemText::parse("user=Q/owner").is_err());
        assert!(AclItemText::parse("user=U").is_err());
    }
}
