/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_UQA_LOG_H
#define UQA_UQA_LOG_H

#include <uqa/uqa.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct uqa_log_record {
    uint32_t struct_size;
    uint32_t version;
    uint32_t level;
    int64_t timestamp_unix_us;
    uqa_string_view target;
    uqa_string_view message;
} uqa_log_record;

typedef uqa_status (UQA_CALL *uqa_log_callback)(
    const uqa_log_record *record, void *user_data);

UQA_API uqa_status UQA_CALL uqa_config_get_log_callback(
    uqa_config config, uqa_log_callback *callback,
    void **user_data, uqa_user_data_destroy_fn *destroy_user_data);
UQA_API uqa_status UQA_CALL uqa_config_set_log_callback(
    uqa_config config, uqa_log_callback callback,
    void *user_data, uqa_user_data_destroy_fn destroy_user_data);
UQA_API uqa_status UQA_CALL uqa_config_get_log_level(
    uqa_config config, uint32_t *level);
UQA_API uqa_status UQA_CALL uqa_config_set_log_level(
    uqa_config config, uint32_t level);

UQA_API uqa_status UQA_CALL uqa_log_get_callback(
    uqa_engine engine, uqa_log_callback *callback,
    void **user_data, uqa_user_data_destroy_fn *destroy_user_data);
UQA_API uqa_status UQA_CALL uqa_log_set_callback(
    uqa_engine engine, uqa_log_callback callback,
    void *user_data, uqa_user_data_destroy_fn destroy_user_data);
UQA_API uqa_status UQA_CALL uqa_log_get_level(
    uqa_engine engine, uint32_t *level);
UQA_API uqa_status UQA_CALL uqa_log_set_level(
    uqa_engine engine, uint32_t level);
UQA_API uqa_status UQA_CALL uqa_log_get_callback_status(
    uqa_engine engine, uqa_status *out);
UQA_API uqa_status UQA_CALL uqa_log_check_callback(uqa_engine engine);

#ifdef __cplusplus
}
#endif

#endif /* UQA_UQA_LOG_H */
