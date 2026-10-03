//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The warning of a `GRANT` or `REVOKE` that changes fewer privileges than it names.

use crate::SQLNotice;

/// The warning `PostgreSQL`'s `restrict_and_check_grant` reports when the grantor holds the grant option for none of the privileges a `GRANT` or `REVOKE` of `name` names, or, when `partial`, for only some of them: SQLSTATE `01007` (`warning_privilege_not_granted`) for a grant and `01006` (`warning_privilege_not_revoked`) for a revocation.
pub fn acl_warning(is_grant: bool, partial: bool, name: &str) -> SQLNotice {
    let (message, sqlstate) = match (is_grant, partial) {
        (true, true) => (
            format!("not all privileges were granted for \"{name}\""),
            "01007",
        ),
        (true, false) => (
            format!("no privileges were granted for \"{name}\""),
            "01007",
        ),
        (false, true) => (
            format!("not all privileges could be revoked for \"{name}\""),
            "01006",
        ),
        (false, false) => (
            format!("no privileges could be revoked for \"{name}\""),
            "01006",
        ),
    };
    SQLNotice::warning(message).with_sqlstate(sqlstate)
}
