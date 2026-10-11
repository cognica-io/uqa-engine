#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Python bindings for the UQA Rust engine."""

from __future__ import annotations

from ._uqa import (
    Engine,
    HttpEngine,
    HttpEngineError,
    HttpSQLStream,
    HttpNotificationOptions,
    NotificationError,
    NotificationEvent,
    NotificationFailure,
    NotificationRetryOptions,
    NotificationSubscription,
    NotificationSubscriptionOptions,
    SQLParam,
    SQLResult,
    __version__,
    detect_database_file,
    migrate_python_db,
    open,
    open_auto,
    open_compressed,
    open_compressed_encrypted,
    open_encrypted,
    scalar,
    tensor,
    vector,
)

from ._notifications import AsyncNotificationSubscription

__all__ = [
    "Engine",
    "HttpEngine",
    "HttpEngineError",
    "HttpSQLStream",
    "HttpNotificationOptions",
    "NotificationError",
    "NotificationEvent",
    "NotificationFailure",
    "NotificationRetryOptions",
    "NotificationSubscription",
    "NotificationSubscriptionOptions",
    "AsyncNotificationSubscription",
    "SQLParam",
    "SQLResult",
    "__version__",
    "detect_database_file",
    "migrate_python_db",
    "open",
    "open_auto",
    "open_compressed",
    "open_compressed_encrypted",
    "open_encrypted",
    "scalar",
    "tensor",
    "vector",
]
