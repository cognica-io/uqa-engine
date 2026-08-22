#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Installed ``usql`` console entry point."""

from __future__ import annotations

from ._uqa import _usql_main


def main() -> int:
    """Run ``usql`` with the current process arguments and standard streams."""
    return _usql_main()
