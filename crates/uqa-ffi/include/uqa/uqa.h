/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_UQA_H
#define UQA_UQA_H

#ifdef __cplusplus
extern "C" {
#endif

#include <uqa/uqa_export.h>
#include <stddef.h>
#include <stdint.h>

typedef struct uqa_engine_t *uqa_engine;
typedef struct uqa_config_t *uqa_config;
typedef const struct uqa_error_t *uqa_error;
typedef struct uqa_result_t *uqa_result;
typedef struct uqa_batch_result_t *uqa_batch_result;
typedef struct uqa_statement_t *uqa_statement;
typedef struct uqa_cursor_t *uqa_cursor;
typedef struct uqa_value_t *uqa_value;
typedef const struct uqa_value_t *uqa_value_view;
typedef const struct uqa_type_t *uqa_type;
typedef struct uqa_buffer_t *uqa_buffer;
typedef struct uqa_cancel_handle_t *uqa_cancel_handle;
typedef struct uqa_subscription_t *uqa_subscription;
typedef struct uqa_notification_t *uqa_notification;
typedef struct uqa_notice_t *uqa_notice;

typedef int32_t uqa_status;
typedef uint32_t uqa_type_kind;
typedef uint32_t uqa_notice_level;
typedef uint32_t uqa_notification_failure_kind;
typedef uint32_t uqa_open_flags;
typedef uint32_t uqa_range_flags;

typedef uqa_status (UQA_CALL *uqa_user_data_destroy_fn)(void *user_data);

typedef struct uqa_string_view {
    const char *data;
    size_t size;
} uqa_string_view;

/* A current live attribute. Views remain valid for the parent type's lifetime. */
typedef struct uqa_field_descriptor {
    uqa_string_view name;
    uqa_type type;
    int32_t number;
    uint32_t collation_oid;
} uqa_field_descriptor;

typedef struct uqa_bytes_view {
    const uint8_t *data;
    size_t size;
} uqa_bytes_view;

typedef struct uqa_bits_view {
    const uint8_t *data;
    size_t size;
    uint32_t bit_length;
} uqa_bits_view;

typedef struct uqa_vector_view {
    const float *data;
    uint32_t dimensions;
} uqa_vector_view;

typedef struct uqa_tensor_view {
    const float *data;
    uint64_t row_count;
    uint32_t dimensions;
} uqa_tensor_view;

typedef struct uqa_array_dimension {
    uint64_t length;
    int32_t lower_bound;
} uqa_array_dimension;

typedef struct uqa_array_shape {
    const uqa_array_dimension *dimensions;
    uint32_t rank;
    uint64_t element_count;
} uqa_array_shape;

typedef struct uqa_range_bounds {
    uqa_value_view lower;
    uqa_value_view upper;
    uqa_range_flags flags;
} uqa_range_bounds;

#define UQA_RANGE_EMPTY UINT32_C(1)
#define UQA_RANGE_LOWER_INCLUSIVE UINT32_C(2)
#define UQA_RANGE_UPPER_INCLUSIVE UINT32_C(4)
#define UQA_RANGE_LOWER_UNBOUNDED UINT32_C(8)
#define UQA_RANGE_UPPER_UNBOUNDED UINT32_C(16)

/* Encrypted compressed storage container identity and exact state.
 * Persist the fields in an external trusted store; this struct is not a wire
 * format. A copied anchor owns no allocation and needs no destruction. */
typedef struct uqa_container_anchor {
    uint8_t database_id[16];
    uint64_t generation;
    uint8_t state_tag[32];
} uqa_container_anchor;

#define UQA_OK                       INT32_C(0)
#define UQA_DONE                     INT32_C(1)
#define UQA_TIMEOUT                  INT32_C(2)
#define UQA_ERROR_INVALID_ARGUMENT  (-INT32_C(1))
#define UQA_ERROR_STATE             (-INT32_C(2))
#define UQA_ERROR_TYPE              (-INT32_C(3))
#define UQA_ERROR_RANGE             (-INT32_C(4))
#define UQA_ERROR_UNSUPPORTED       (-INT32_C(5))
#define UQA_ERROR_BUSY              (-INT32_C(6))
#define UQA_ERROR_OUT_OF_MEMORY     (-INT32_C(7))
#define UQA_ERROR_SQL               (-INT32_C(8))
#define UQA_ERROR_IO                (-INT32_C(9))
#define UQA_ERROR_PANIC             (-INT32_C(10))
#define UQA_ERROR_LIMIT             (-INT32_C(11))
#define UQA_ERROR_NULL              (-INT32_C(12))
#define UQA_ERROR_NOTIFICATION      (-INT32_C(13))

#define UQA_NOTIFICATION_FAILURE_INVALID_REQUEST UINT32_C(1)
#define UQA_NOTIFICATION_FAILURE_AUTHENTICATION UINT32_C(2)
#define UQA_NOTIFICATION_FAILURE_AUTHORITY_REVOKED UINT32_C(3)
#define UQA_NOTIFICATION_FAILURE_UNSUPPORTED UINT32_C(4)
#define UQA_NOTIFICATION_FAILURE_CAPACITY UINT32_C(5)
#define UQA_NOTIFICATION_FAILURE_BACKPRESSURE UINT32_C(6)
#define UQA_NOTIFICATION_FAILURE_PROTOCOL UINT32_C(7)
#define UQA_NOTIFICATION_FAILURE_SOURCE_UNAVAILABLE UINT32_C(8)
#define UQA_NOTIFICATION_FAILURE_TRANSPORT UINT32_C(9)
#define UQA_NOTIFICATION_FAILURE_TIMEOUT UINT32_C(10)
#define UQA_NOTIFICATION_FAILURE_SERVER_DRAINING UINT32_C(11)
#define UQA_NOTIFICATION_FAILURE_CANCELLED UINT32_C(12)
#define UQA_NOTIFICATION_FAILURE_SEQUENCE_EXHAUSTED UINT32_C(13)

#define UQA_COMPLETION_NONE UINT32_C(0)
#define UQA_COMPLETION_COMMITTED UINT32_C(1)
#define UQA_COMPLETION_ROLLED_BACK UINT32_C(2)
#define UQA_COMPLETION_PENDING UINT32_C(3)
#define UQA_COMPLETION_IN_TRANSACTION UINT32_C(4)
#define UQA_COMPLETION_UNKNOWN UINT32_C(5)

#define UQA_COMPRESSION_NONE UINT32_C(0)
#define UQA_COMPRESSION_ZSTD UINT32_C(1)
#define UQA_COMPRESSION_LZ4 UINT32_C(2)

#define UQA_OPEN_MEMORY        (UINT32_C(1) << 0)
#define UQA_OPEN_SQLITE_NATIVE (UINT32_C(1) << 1)
#define UQA_OPEN_SQLITE_KV     (UINT32_C(1) << 2)
#define UQA_OPEN_REDB          (UINT32_C(1) << 3)
#define UQA_OPEN_SQLITE_AUTO   (UINT32_C(1) << 4)

#define UQA_TRANSACTION_IDLE UINT32_C(0)
#define UQA_TRANSACTION_ACTIVE UINT32_C(1)
#define UQA_TRANSACTION_FAILED UINT32_C(2)
#define UQA_TRANSACTION_COMPLETION_PENDING UINT32_C(3)

#define UQA_RESULT_COMMAND UINT32_C(0)
#define UQA_RESULT_ROWS UINT32_C(1)
#define UQA_RESULT_UNKNOWN UINT32_C(2)

#define UQA_NOTICE_LEVEL_DEBUG UINT32_C(0)
#define UQA_NOTICE_LEVEL_LOG UINT32_C(1)
#define UQA_NOTICE_LEVEL_INFO UINT32_C(2)
#define UQA_NOTICE_LEVEL_NOTICE UINT32_C(3)
#define UQA_NOTICE_LEVEL_WARNING UINT32_C(4)

#define UQA_TYPE_UNKNOWN UINT32_C(0)
#define UQA_TYPE_BOOL UINT32_C(1)
#define UQA_TYPE_INT16 UINT32_C(2)
#define UQA_TYPE_INT32 UINT32_C(3)
#define UQA_TYPE_INT64 UINT32_C(4)
#define UQA_TYPE_FLOAT32 UINT32_C(5)
#define UQA_TYPE_FLOAT64 UINT32_C(6)
#define UQA_TYPE_TEXT UINT32_C(7)
#define UQA_TYPE_CHAR UINT32_C(8)
#define UQA_TYPE_VARCHAR UINT32_C(9)
#define UQA_TYPE_BYTEA UINT32_C(10)
#define UQA_TYPE_NUMERIC UINT32_C(11)
#define UQA_TYPE_DATE UINT32_C(12)
#define UQA_TYPE_TIME UINT32_C(13)
#define UQA_TYPE_TIMETZ UINT32_C(14)
#define UQA_TYPE_TIMESTAMP UINT32_C(15)
#define UQA_TYPE_TIMESTAMPTZ UINT32_C(16)
#define UQA_TYPE_INTERVAL UINT32_C(17)
#define UQA_TYPE_JSON UINT32_C(18)
#define UQA_TYPE_JSONB UINT32_C(19)
#define UQA_TYPE_UUID UINT32_C(20)
#define UQA_TYPE_BIT UINT32_C(21)
#define UQA_TYPE_VARBIT UINT32_C(22)
#define UQA_TYPE_ARRAY UINT32_C(23)
#define UQA_TYPE_RECORD UINT32_C(24)
#define UQA_TYPE_ENUM UINT32_C(25)
#define UQA_TYPE_DOMAIN UINT32_C(26)
#define UQA_TYPE_RANGE UINT32_C(27)
#define UQA_TYPE_MULTIRANGE UINT32_C(28)
#define UQA_TYPE_VECTOR UINT32_C(29)
#define UQA_TYPE_TENSOR UINT32_C(30)
#define UQA_TYPE_VOID UINT32_C(31)
#define UQA_TYPE_OID UINT32_C(32)
#define UQA_TYPE_XID UINT32_C(33)
#define UQA_TYPE_NAME UINT32_C(34)
#define UQA_TYPE_INTERNAL_CHAR UINT32_C(35)
#define UQA_TYPE_REFCURSOR UINT32_C(36)
#define UQA_TYPE_REGPROC UINT32_C(37)
#define UQA_TYPE_REGPROCEDURE UINT32_C(38)
#define UQA_TYPE_REGCLASS UINT32_C(39)
#define UQA_TYPE_REGNAMESPACE UINT32_C(40)
#define UQA_TYPE_REGCOLLATION UINT32_C(41)
#define UQA_TYPE_REGROLE UINT32_C(42)
#define UQA_TYPE_REGTYPE UINT32_C(43)
#define UQA_TYPE_PG_NODE_TREE UINT32_C(44)
#define UQA_TYPE_ACL_ITEM UINT32_C(45)
#define UQA_TYPE_INT2VECTOR UINT32_C(46)
#define UQA_TYPE_OIDVECTOR UINT32_C(47)
#define UQA_TYPE_ANYARRAY UINT32_C(48)
#define UQA_TYPE_COMPOSITE UINT32_C(49)
#define UQA_TYPE_XML UINT32_C(50)

#define UQA_CAP_NORI (UINT64_C(1) << 0)
#define UQA_CAP_KUROMOJI (UINT64_C(1) << 1)
#define UQA_CAP_ARROW_FDW (UINT64_C(1) << 2)
#define UQA_CAP_DUCKDB_FDW (UINT64_C(1) << 3)
#define UQA_CAP_ARROW_EXPORT (UINT64_C(1) << 4)
#define UQA_CAP_PERSISTENCE (UINT64_C(1) << 5)
#define UQA_CAP_INDEPENDENT_SESSIONS (UINT64_C(1) << 6)

UQA_API uqa_status UQA_CALL uqa_library_get_abi_version(uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_library_get_version(uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_library_get_source_revision(uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_library_get_capabilities(uint64_t *out);

UQA_API uqa_status UQA_CALL uqa_config_destroy(uqa_config *config);
UQA_API uqa_status UQA_CALL uqa_result_destroy(uqa_result *result);
UQA_API uqa_status UQA_CALL uqa_value_destroy(uqa_value *value);
UQA_API uqa_status UQA_CALL uqa_buffer_destroy(uqa_buffer *buffer);
UQA_API uqa_status UQA_CALL uqa_engine_close(uqa_engine *engine);

UQA_API uqa_status UQA_CALL uqa_error_get_last(uqa_error *out);
UQA_API uqa_status UQA_CALL uqa_error_get_status(uqa_error error, uqa_status *out);
UQA_API uqa_status UQA_CALL uqa_error_get_cause(uqa_error error, uqa_error *out);
UQA_API uqa_status UQA_CALL uqa_error_get_notification_kind(
    uqa_error error, uqa_notification_failure_kind *out);
UQA_API uqa_status UQA_CALL uqa_error_get_notification_code(
    uqa_error error, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_error_get_sqlstate(uqa_error error, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_error_get_message(uqa_error error, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_error_get_detail(uqa_error error, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_error_get_hint(uqa_error error, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_error_get_context(uqa_error error, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_error_get_completion(uqa_error error, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_error_get_statement_index(
    uqa_error error, uint64_t *index);
UQA_API uqa_status UQA_CALL uqa_error_get_position(
    uqa_error error, uint64_t *character_position);

UQA_API uqa_status UQA_CALL uqa_config_create(uqa_config *out);
UQA_API uqa_status UQA_CALL uqa_config_get_encryption_key(
    uqa_config config, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_config_set_encryption_key(
    uqa_config config, const char *key);
UQA_API uqa_status UQA_CALL uqa_config_set_encryption_key_n(
    uqa_config config, const char *key, size_t key_size);
UQA_API uqa_status UQA_CALL uqa_config_get_compression(
    uqa_config config, uint32_t *codec, int32_t *level);
UQA_API uqa_status UQA_CALL uqa_config_set_compression(
    uqa_config config, uint32_t codec, int32_t level);
/* The setter copies an anchor; NULL clears it. An unset getter returns DONE.
 * An anchor requires a key and compressed SQLite, explicitly selected or
 * detected by SQLITE_AUTO. It cannot create a new container. */
UQA_API uqa_status UQA_CALL uqa_config_get_trusted_anchor(
    uqa_config config, uqa_container_anchor *out);
UQA_API uqa_status UQA_CALL uqa_config_set_trusted_anchor(
    uqa_config config, const uqa_container_anchor *anchor);
/* Authenticate the on-disk committed state without opening an Engine. Read
 * after closing writers, then retain the result externally before reopening.
 * Wrong keys, unencrypted containers and other file formats fail. */
UQA_API uqa_status UQA_CALL uqa_container_get_anchor(
    const char *path, const char *key, uqa_container_anchor *out);
UQA_API uqa_status UQA_CALL uqa_container_get_anchor_n(
    const char *path, size_t path_size, const char *key, size_t key_size,
    uqa_container_anchor *out);
UQA_API uqa_status UQA_CALL uqa_config_get_ffi_memory_limit(
    uqa_config config, uint64_t *bytes);
UQA_API uqa_status UQA_CALL uqa_config_set_ffi_memory_limit(
    uqa_config config, uint64_t bytes);

/* _n string lengths count UTF-8 bytes, excluding a terminator. No terminator
 * is required; embedded NUL and invalid UTF-8 are rejected. Required paths,
 * role names and savepoint names remain non-NULL even at length zero.
 * Memory open requires a NULL path and, for open_n, a zero path_size.
 * Inputs need remain valid only until the operation returns. */
UQA_API uqa_status UQA_CALL uqa_engine_open(
    const char *path, uqa_open_flags flags, uqa_config config,
    uqa_engine *out);
UQA_API uqa_status UQA_CALL uqa_engine_open_n(
    const char *path, size_t path_size, uqa_open_flags flags,
    uqa_config config, uqa_engine *out);
UQA_API uqa_status UQA_CALL uqa_engine_open_session(
    uqa_engine source, uqa_engine *out);
UQA_API uqa_status UQA_CALL uqa_engine_open_session_for_user(
    uqa_engine source, const char *role,
    uqa_engine *out);
UQA_API uqa_status UQA_CALL uqa_engine_open_session_for_user_n(
    uqa_engine source, const char *role, size_t role_size,
    uqa_engine *out);
UQA_API uqa_status UQA_CALL uqa_engine_get_capabilities(
    uqa_engine engine, uint64_t *out);

typedef struct uqa_sql_request {
    const char *sql;
    const uqa_value_view *params;
    uint32_t param_count;
} uqa_sql_request;

typedef struct uqa_sql_request_n {
    const char *sql;
    size_t sql_size;
    const uqa_value_view *params;
    uint32_t param_count;
} uqa_sql_request_n;

UQA_API uqa_status UQA_CALL uqa_engine_execute(
    uqa_engine engine, const char *sql,
    uqa_result result);
UQA_API uqa_status UQA_CALL uqa_engine_execute_n(
    uqa_engine engine, const char *sql, size_t sql_size,
    uqa_result result);
UQA_API uqa_status UQA_CALL uqa_engine_execute_params(
    uqa_engine engine, const char *sql,
    const uqa_value_view *params, uint32_t param_count,
    uqa_result result);
UQA_API uqa_status UQA_CALL uqa_engine_execute_params_n(
    uqa_engine engine, const char *sql, size_t sql_size,
    const uqa_value_view *params, uint32_t param_count,
    uqa_result result);
UQA_API uqa_status UQA_CALL uqa_batch_result_create(uqa_batch_result *out);

UQA_API uqa_status UQA_CALL uqa_engine_execute_batch(
    uqa_engine engine, const uqa_sql_request *requests, size_t count,
    uqa_batch_result results);
UQA_API uqa_status UQA_CALL uqa_engine_execute_batch_n(
    uqa_engine engine, const uqa_sql_request_n *requests, size_t count,
    uqa_batch_result results);
UQA_API uqa_status UQA_CALL uqa_batch_result_get_count(
    uqa_batch_result batch, uint64_t *out);
UQA_API uqa_status UQA_CALL uqa_batch_result_take(
    uqa_batch_result batch, uint64_t index,
    uqa_result result);
UQA_API uqa_status UQA_CALL uqa_batch_result_destroy(uqa_batch_result *batch);

UQA_API uqa_status UQA_CALL uqa_transaction_begin(uqa_engine engine);
UQA_API uqa_status UQA_CALL uqa_transaction_commit(uqa_engine engine);
UQA_API uqa_status UQA_CALL uqa_transaction_rollback(uqa_engine engine);
UQA_API uqa_status UQA_CALL uqa_savepoint_create(
    uqa_engine engine, const char *name);
UQA_API uqa_status UQA_CALL uqa_savepoint_create_n(
    uqa_engine engine, const char *name, size_t name_size);
UQA_API uqa_status UQA_CALL uqa_savepoint_rollback(
    uqa_engine engine, const char *name);
UQA_API uqa_status UQA_CALL uqa_savepoint_rollback_n(
    uqa_engine engine, const char *name, size_t name_size);
UQA_API uqa_status UQA_CALL uqa_savepoint_release(
    uqa_engine engine, const char *name);
UQA_API uqa_status UQA_CALL uqa_savepoint_release_n(
    uqa_engine engine, const char *name, size_t name_size);
UQA_API uqa_status UQA_CALL uqa_transaction_get_state(
    uqa_engine engine, uint32_t *out_state);

UQA_API uqa_status UQA_CALL uqa_result_create(uqa_result *out);

UQA_API uqa_status UQA_CALL uqa_result_get_row_count(
    uqa_result result, uint64_t *out);
UQA_API uqa_status UQA_CALL uqa_result_get_column_count(
    uqa_result result, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_result_get_column_name(
    uqa_result result, uint32_t column,
    uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_result_get_column_type(
    uqa_result result, uint32_t column,
    uqa_type *out);
UQA_API uqa_status UQA_CALL uqa_result_get_value(
    uqa_result result, uint64_t row, uint32_t column,
    uqa_value_view *out);
UQA_API uqa_status UQA_CALL uqa_result_get_affected_rows(
    uqa_result result, uint64_t *out);
UQA_API uqa_status UQA_CALL uqa_result_get_command_tag(
    uqa_result result, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_result_get_kind(
    uqa_result result, uint32_t *out);

UQA_API uqa_status UQA_CALL uqa_type_get_kind(
    uqa_type type, uqa_type_kind *out);
UQA_API uqa_status UQA_CALL uqa_type_get_oid(
    uqa_type type, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_type_get_modifier(
    uqa_type type, int32_t *out);
UQA_API uqa_status UQA_CALL uqa_type_get_collation_oid(
    uqa_type type, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_type_get_numeric_precision(uqa_type type, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_type_get_numeric_scale(uqa_type type, int32_t *out);
UQA_API uqa_status UQA_CALL uqa_type_get_dimensions(uqa_type type, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_type_get_name(uqa_type type, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_type_get_schema(uqa_type type, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_type_get_base_type(uqa_type type, uqa_type *out);
UQA_API uqa_status UQA_CALL uqa_type_get_element_type(uqa_type type, uqa_type *out);
/* Subtype applies to ranges and multiranges; range_type applies to multiranges.
 * Domains expose these through their base type. Other types return UQA_DONE. */
UQA_API uqa_status UQA_CALL uqa_type_get_range_subtype(uqa_type type, uqa_type *out);
UQA_API uqa_status UQA_CALL uqa_type_get_range_type(uqa_type type, uqa_type *out);
/* Live zero-based fields of a named composite; other types return UQA_DONE.
 * Domains expose fields through their base type. Dropped slots are excluded;
 * number retains pg_attribute.attnum. Earlier views survive catalog changes. */
UQA_API uqa_status UQA_CALL uqa_type_get_field_count(uqa_type type, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_type_get_field(
    uqa_type type, uint32_t index, uqa_field_descriptor *out);

/* Constructors copy inputs and preserve their SQL type, including integer and
 * floating widths. NULL from create_null has no declared type. */
UQA_API uqa_status UQA_CALL uqa_value_create_null(uqa_value *out);

typedef uint32_t uqa_temporal_kind;
#define UQA_TEMPORAL_FINITE UINT32_C(0)
#define UQA_TEMPORAL_NEGATIVE_INFINITY UINT32_C(1)
#define UQA_TEMPORAL_POSITIVE_INFINITY UINT32_C(2)

/* DATE days and timestamp microseconds are relative to 2000-01-01.
 * Infinite inputs ignore the payload; getters return a zero payload.
 * Reserved fields must be zero. Finite constructors validate SQL input ranges;
 * getters preserve admitted physical values without re-running type input. */
typedef struct uqa_date {
    int32_t days;
    uqa_temporal_kind kind;
} uqa_date;

typedef struct uqa_timestamp {
    int64_t micros;
    uqa_temporal_kind kind;
    uint32_t reserved;
} uqa_timestamp;

typedef struct uqa_timetz {
    int64_t micros;
    int32_t offset_seconds;
    uint32_t reserved;
} uqa_timetz;

typedef struct uqa_interval {
    int64_t micros;
    int32_t days;
    int32_t months;
    uqa_temporal_kind kind;
    uint32_t reserved;
} uqa_interval;

UQA_API uqa_status UQA_CALL uqa_value_create_date(uqa_date value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_time(int64_t micros, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_timetz(uqa_timetz value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_timestamp(uqa_timestamp value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_timestamptz(uqa_timestamp value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_interval(uqa_interval value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_get_date(uqa_value_view value, uqa_date *out);
UQA_API uqa_status UQA_CALL uqa_value_get_time(uqa_value_view value, int64_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_timetz(uqa_value_view value, uqa_timetz *out);
UQA_API uqa_status UQA_CALL uqa_value_get_timestamp(uqa_value_view value, uqa_timestamp *out);
UQA_API uqa_status UQA_CALL uqa_value_get_timestamptz(uqa_value_view value, uqa_timestamp *out);
UQA_API uqa_status UQA_CALL uqa_value_get_interval(uqa_value_view value, uqa_interval *out);

/* Catalog-aware constructors execute native type input in the Engine's
 * statement/transaction scope and retain the session until value destruction.
 * Type names include native SQL modifiers; NULL input still checks domains.
 * Required string pointers remain non-NULL in the _n forms, including an
 * empty text input. Use create_typed_null for SQL NULL. */
UQA_API uqa_status UQA_CALL uqa_value_create_typed_null(
    uqa_engine engine, const char *type_name, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_typed_null_n(
    uqa_engine engine, const char *type_name, size_t type_name_size, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_from_text(
    uqa_engine engine, const char *type_name, const char *text, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_from_text_n(
    uqa_engine engine, const char *type_name, size_t type_name_size,
    const char *text, size_t text_size, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_int16(int16_t value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_int32(int32_t value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_int64(int64_t value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_oid(uint32_t value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_xid(uint32_t value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_void(uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_internal_char(uint8_t value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_xml(
    const char *value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_xml_n(
    const char *value, size_t size, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_bool(uint8_t value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_float32(float value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_float64(double value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_bytes(
    const uint8_t *data, size_t size, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_text(
    const char *value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_text_n(
    const char *value, size_t size, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_numeric(
    const char *value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_numeric_n(
    const char *value, size_t size, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_json(
    const char *value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_json_n(
    const char *value, size_t size, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_jsonb(
    const char *value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_jsonb_n(
    const char *value, size_t size, uqa_value *out);
/* UUID text uses native input rules; byte input contains exactly sixteen
 * network-order octets, with no UUID version or variant restriction. */
UQA_API uqa_status UQA_CALL uqa_value_create_uuid(
    const char *value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_uuid_n(
    const char *value, size_t size, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_uuid_bytes(
    const uint8_t *data, size_t size, uqa_value *out);
/* Packed bytes are MSB-first; size must equal ceil(bit_length / 8).
 * Unused low bits are discarded. Empty input permits NULL with size zero.
 * The type has no length modifier; logical length belongs to the value. */
UQA_API uqa_status UQA_CALL uqa_value_create_bit(
    const uint8_t *data, size_t size, uint32_t bit_length, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_varbit(
    const uint8_t *data, size_t size, uint32_t bit_length, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_vector_f32(
    const float *data, size_t dimensions,
    uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_tensor_f32(
    const float *data, uint64_t row_count, uint32_t dimensions, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_get_vector_f32(
    uqa_value_view value, uqa_vector_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_tensor_f32(
    uqa_value_view value, uqa_tensor_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_array_shape(
    uqa_value_view value, uqa_array_shape *out);
/* Construct a named composite/domain or anonymous record from positional fields.
 * Inputs are copied before native execution; SQL NULL uses a value handle. */
UQA_API uqa_status UQA_CALL uqa_value_create_record(
    uqa_engine engine, const char *type_name,
    const uqa_value_view *fields, uint32_t field_count, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_record_n(
    uqa_engine engine, const char *type_name, size_t type_name_size,
    const uqa_value_view *fields, uint32_t field_count, uqa_value *out);
/* type_name is an array declaration; elements are copied in row-major order. */
UQA_API uqa_status UQA_CALL uqa_value_create_array(
    uqa_engine engine, const char *type_name,
    const uqa_array_dimension *dimensions, uint32_t rank,
    const uqa_value_view *elements, uint64_t element_count, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_array_n(
    uqa_engine engine, const char *type_name, size_t type_name_size,
    const uqa_array_dimension *dimensions, uint32_t rank,
    const uqa_value_view *elements, uint64_t element_count, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_get_element(
    uqa_value_view value, uint64_t index, uqa_value_view *out);
/* Record positions are zero-based and exclude dropped attributes. Children
 * borrow the parent; each published child retains its selected type and value
 * even when a later observation uses a changed composite descriptor. */
UQA_API uqa_status UQA_CALL uqa_value_get_field_count(
    uqa_value_view value, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_field(
    uqa_value_view value, uint32_t index, uqa_value_view *out);
/* A NULL endpoint pointer is unbounded. An explicit UNBOUNDED flag requires a
 * NULL pointer. EMPTY requires two NULL pointers and no other flags. SQL NULL
 * endpoints also become unbounded after native type conversion. */
UQA_API uqa_status UQA_CALL uqa_value_create_range(
    uqa_engine engine, const char *type_name, uqa_value_view lower,
    uqa_value_view upper, uqa_range_flags flags, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_range_n(
    uqa_engine engine, const char *type_name, size_t type_name_size,
    uqa_value_view lower, uqa_value_view upper, uqa_range_flags flags,
    uqa_value *out);
/* Members are copied, ordered and merged by SQL. SQL NULL members fail. */
UQA_API uqa_status UQA_CALL uqa_value_create_multirange(
    uqa_engine engine, const char *type_name, const uqa_value_view *ranges,
    uint32_t range_count, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_create_multirange_n(
    uqa_engine engine, const char *type_name, size_t type_name_size,
    const uqa_value_view *ranges, uint32_t range_count, uqa_value *out);
/* Bounds and members borrow their parent. An empty range has only EMPTY set;
 * a present infinite endpoint is a value, distinct from an unbounded endpoint. */
UQA_API uqa_status UQA_CALL uqa_value_get_range_bounds(
    uqa_value_view value, uqa_range_bounds *out);
UQA_API uqa_status UQA_CALL uqa_value_get_range_count(
    uqa_value_view value, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_range(
    uqa_value_view value, uint32_t index, uqa_value_view *out);
UQA_API uqa_status UQA_CALL uqa_value_create_copy(
    uqa_value_view value, uqa_value *out);
UQA_API uqa_status UQA_CALL uqa_value_get_is_null(
    uqa_value_view value, uint8_t *out);
/* The descriptor borrows the value's lifetime. Untyped NULL returns UQA_DONE
 * with a NULL descriptor. Getters allow integer/float widening, never narrowing
 * or text/numeric coercion; SQL NULL returns UQA_ERROR_NULL. */
UQA_API uqa_status UQA_CALL uqa_value_get_type(
    uqa_value_view value, uqa_type *out);
UQA_API uqa_status UQA_CALL uqa_value_get_int16(
    uqa_value_view value, int16_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_int32(
    uqa_value_view value, int32_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_int64(
    uqa_value_view value, int64_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_oid(uqa_value_view value, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_xid(uqa_value_view value, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_void(uqa_value_view value);
UQA_API uqa_status UQA_CALL uqa_value_get_internal_char(uqa_value_view value, uint8_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_xml(
    uqa_value_view value, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_bool(uqa_value_view value, uint8_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_float32(uqa_value_view value, float *out);
UQA_API uqa_status UQA_CALL uqa_value_get_float64(uqa_value_view value, double *out);
/* NUMERIC text is exact and keeps its display scale. Special numeric values
 * have no display scale: get_numeric_scale returns UQA_DONE with zero output. */
UQA_API uqa_status UQA_CALL uqa_value_get_numeric(uqa_value_view value, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_numeric_scale(uqa_value_view value, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_value_get_json(uqa_value_view value, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_jsonb(uqa_value_view value, uqa_string_view *out);
/* Independently constructed immutable values permit concurrent readers;
 * synchronize destruction against every use. Session-backed values retain
 * session admission and can return UQA_ERROR_BUSY during another operation.
 * These views borrow the value's lifetime. UUID octets are in
 * network order; UUID text is canonical. BIT and VARBIT accessors are distinct. */
UQA_API uqa_status UQA_CALL uqa_value_get_uuid_bytes(uqa_value_view value, uqa_bytes_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_uuid(uqa_value_view value, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_bit(uqa_value_view value, uqa_bits_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_varbit(uqa_value_view value, uqa_bits_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_bytes(uqa_value_view value, uqa_bytes_view *out);
UQA_API uqa_status UQA_CALL uqa_value_get_text(
    uqa_value_view value, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_buffer_create(uqa_buffer *out);
UQA_API uqa_status UQA_CALL uqa_value_format_sql(
    uqa_value_view value, uqa_buffer buffer);
UQA_API uqa_status UQA_CALL uqa_buffer_get_bytes(uqa_buffer buffer, uqa_bytes_view *out);

UQA_API uqa_status UQA_CALL uqa_statement_prepare(
    uqa_engine engine, const char *sql,
    uqa_statement *out);
UQA_API uqa_status UQA_CALL uqa_statement_prepare_n(
    uqa_engine engine, const char *sql, size_t sql_size,
    uqa_statement *out);
/* Explicit SQL type names are copied during prepare; "unknown" requests inference.
 * Parameter indices start at 1; result column indices start at 0. Descriptor
 * views remain valid until finalize. Missing bindings differ from SQL NULL. */
UQA_API uqa_status UQA_CALL uqa_statement_prepare_typed(
    uqa_engine engine, const char *sql,
    const char *const *parameter_types, uint32_t parameter_count,
    uqa_statement *out);
UQA_API uqa_status UQA_CALL uqa_statement_prepare_typed_n(
    uqa_engine engine, const char *sql, size_t sql_size,
    const char *const *parameter_types, const size_t *parameter_type_sizes,
    uint32_t parameter_count, uqa_statement *out);
UQA_API uqa_status UQA_CALL uqa_statement_get_parameter_count(
    uqa_statement statement, uint32_t *out);
/* Unresolved input types return UQA_DONE with a NULL descriptor. Empty input
 * can retain an unused "unknown" declaration; its parameter still needs binding. */
UQA_API uqa_status UQA_CALL uqa_statement_get_parameter_type(
    uqa_statement statement, uint32_t index, uqa_type *out);
UQA_API uqa_status UQA_CALL uqa_statement_get_column_count(
    uqa_statement statement, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_statement_get_column_name(
    uqa_statement statement, uint32_t column, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_statement_get_column_type(
    uqa_statement statement, uint32_t column, uqa_type *out);
UQA_API uqa_status UQA_CALL uqa_statement_bind(
    uqa_statement statement, uint32_t index,
    uqa_value_view value);
UQA_API uqa_status UQA_CALL uqa_statement_clear_bindings(
    uqa_statement statement);
UQA_API uqa_status UQA_CALL uqa_statement_execute(
    uqa_statement statement, uqa_result result);
UQA_API uqa_status UQA_CALL uqa_statement_finalize(uqa_statement *statement);

UQA_API uqa_status UQA_CALL uqa_cursor_open(
    uqa_engine engine, const char *sql,
    uqa_cursor *out);
UQA_API uqa_status UQA_CALL uqa_cursor_open_n(
    uqa_engine engine, const char *sql, size_t sql_size,
    uqa_cursor *out);
UQA_API uqa_status UQA_CALL uqa_cursor_open_params(
    uqa_engine engine, const char *sql,
    const uqa_value_view *params, uint32_t param_count,
    uqa_cursor *out);
UQA_API uqa_status UQA_CALL uqa_cursor_open_params_n(
    uqa_engine engine, const char *sql, size_t sql_size,
    const uqa_value_view *params, uint32_t param_count,
    uqa_cursor *out);
UQA_API uqa_status UQA_CALL uqa_cursor_fetch(
    uqa_cursor cursor, uint32_t max_rows,
    uqa_result result);
UQA_API uqa_status UQA_CALL uqa_cursor_get_row_count(
    uqa_cursor cursor, uint64_t *out);
UQA_API uqa_status UQA_CALL uqa_cursor_get_column_count(
    uqa_cursor cursor, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_cursor_get_column_name(
    uqa_cursor cursor, uint32_t column, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_cursor_get_column_type(
    uqa_cursor cursor, uint32_t column,
    uqa_type *out);
UQA_API uqa_status UQA_CALL uqa_cursor_close(uqa_cursor *cursor);

UQA_API uqa_status UQA_CALL uqa_cancel_handle_create(
    uqa_engine engine, uqa_cancel_handle *out);
UQA_API uqa_status UQA_CALL uqa_cancel_handle_request(uqa_cancel_handle handle);
UQA_API uqa_status UQA_CALL uqa_cancel_handle_destroy(uqa_cancel_handle *handle);
UQA_API uqa_status UQA_CALL uqa_engine_reset_cancellation(
    uqa_engine engine);
UQA_API uqa_status UQA_CALL uqa_engine_check_termination(uqa_engine engine);

UQA_API uqa_status UQA_CALL uqa_callback_fail(uqa_status status, const char *message);
UQA_API uqa_status UQA_CALL uqa_callback_fail_n(
    uqa_status status, const char *message, size_t message_size);

typedef uqa_status (UQA_CALL *uqa_copy_read_fn)(
    void *user_data, uint8_t *buffer, size_t capacity,
    size_t *bytes_read);
typedef uqa_status (UQA_CALL *uqa_copy_write_fn)(
    void *user_data, const uint8_t *buffer, size_t size,
    size_t *bytes_written);
typedef uqa_status (UQA_CALL *uqa_copy_flush_fn)(
    void *user_data);

UQA_API uqa_status UQA_CALL uqa_engine_copy_from(
    uqa_engine engine, const char *statement,
    uqa_copy_read_fn read, void *user_data,
    uint64_t *rows);
UQA_API uqa_status UQA_CALL uqa_engine_copy_to(
    uqa_engine engine, const char *statement,
    uqa_copy_write_fn write, uqa_copy_flush_fn flush, void *user_data,
    uint64_t *rows);
UQA_API uqa_status UQA_CALL uqa_engine_copy_from_n(
    uqa_engine engine, const char *statement, size_t statement_size,
    uqa_copy_read_fn read, void *user_data,
    uint64_t *rows);
UQA_API uqa_status UQA_CALL uqa_engine_copy_to_n(
    uqa_engine engine, const char *statement, size_t statement_size,
    uqa_copy_write_fn write, uqa_copy_flush_fn flush, void *user_data,
    uint64_t *rows);

#define UQA_SUBSCRIPTION_OPTIONS_VERSION UINT32_C(1)
#define UQA_SQL_NOTIFICATION_OPTIONS_VERSION UINT32_C(1)

typedef struct uqa_subscription_options {
    uint32_t struct_size;
    uint32_t version;
    uint64_t max_active_subscriptions;
    uint64_t max_channels;
    uint64_t max_queued_notifications;
    uint64_t max_queued_bytes;
    uint64_t max_registry_entries_per_poll;
} uqa_subscription_options;

UQA_API uqa_status UQA_CALL uqa_subscription_open(
    uqa_engine engine, const char *const *channels, size_t count,
    const uqa_subscription_options *options,
    uqa_subscription *out);
UQA_API uqa_status UQA_CALL uqa_subscription_open_n(
    uqa_engine engine, const char *const *channels, const size_t *channel_sizes,
    size_t count, const uqa_subscription_options *options,
    uqa_subscription *out);
UQA_API uqa_status UQA_CALL uqa_notification_create(uqa_notification *out);
UQA_API uqa_status UQA_CALL uqa_subscription_poll(
    uqa_subscription subscription,
    uqa_notification notification);
UQA_API uqa_status UQA_CALL uqa_subscription_wait(
    uqa_subscription subscription, uint64_t timeout_ms,
    uqa_notification notification);
UQA_API uqa_status UQA_CALL uqa_subscription_close(uqa_subscription *subscription);
UQA_API uqa_status UQA_CALL uqa_notification_destroy(uqa_notification *notification);
UQA_API uqa_status UQA_CALL uqa_notification_get_channel(
    uqa_notification notification, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_notification_get_payload(
    uqa_notification notification, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_notification_get_sender_pid(
    uqa_notification notification, int32_t *out);
UQA_API uqa_status UQA_CALL uqa_notification_get_epoch(
    uqa_notification notification, uqa_bytes_view *out);
UQA_API uqa_status UQA_CALL uqa_notification_get_sequence(
    uqa_notification notification, uint64_t *out);

typedef struct uqa_sql_notification_options {
    uint32_t struct_size;
    uint32_t version;
    uint64_t max_channels;
    uint64_t max_queued_notifications;
    uint64_t max_queued_bytes;
    uint64_t max_registry_entries_per_poll;
} uqa_sql_notification_options;

UQA_API uqa_status UQA_CALL uqa_engine_configure_sql_notifications(
    uqa_engine engine, const uqa_sql_notification_options *options);
UQA_API uqa_status UQA_CALL uqa_engine_poll_sql_notification(
    uqa_engine engine, uqa_notification notification);

UQA_API uqa_status UQA_CALL uqa_notice_create(
    uqa_engine engine, uqa_notice *out);
UQA_API uqa_status UQA_CALL uqa_notice_destroy(uqa_notice *notice);
UQA_API uqa_status UQA_CALL uqa_notice_get_level(
    uqa_notice notice, uqa_notice_level *out);
UQA_API uqa_status UQA_CALL uqa_notice_get_sqlstate(
    uqa_notice notice, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_notice_get_message(
    uqa_notice notice, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_notice_get_detail(
    uqa_notice notice, uqa_string_view *out);
UQA_API uqa_status UQA_CALL uqa_notice_get_hint(
    uqa_notice notice, uqa_string_view *out);

#ifdef __cplusplus
}
#endif

#endif /* UQA_UQA_H */
