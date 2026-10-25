/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "allocation.h"
#include <libxml/parser.h>
#include <libxml/xmlmemory.h>
#include <libxml/xmlerror.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#if defined(_MSC_VER)
static __declspec(thread) uqa_sql_xml_control *active_control;
#else
static _Thread_local uqa_sql_xml_control *active_control;
#endif

typedef union allocation {
    struct {
        size_t size;
        uqa_sql_xml_control *owner;
    } value;
    long double alignment;
    void *pointer_alignment;
} allocation;

static void *allocate(size_t bytes) {
    if (bytes > SIZE_MAX - sizeof(allocation)) return NULL;
    size_t total = sizeof(allocation) + bytes;
    uqa_sql_xml_control *owner = active_control;
    if (owner != NULL && owner->reserve(owner->context, total) != 0) return NULL;
    allocation *block = malloc(total);
    if (block == NULL) {
        if (owner != NULL) owner->release(owner->context, total);
        return NULL;
    }
    block->value.size = total;
    block->value.owner = owner;
    return block + 1;
}

static void release(void *pointer) {
    if (pointer == NULL) return;
    allocation *block = (allocation *)pointer - 1;
    uqa_sql_xml_control *owner = block->value.owner;
    if (owner != NULL) owner->release(owner->context, block->value.size);
    free(block);
}

static void *resize(void *pointer, size_t bytes) {
    if (pointer == NULL) return allocate(bytes);
    if (bytes > SIZE_MAX - sizeof(allocation)) return NULL;
    allocation *block = (allocation *)pointer - 1;
    uqa_sql_xml_control *owner = block->value.owner;
    size_t previous = block->value.size;
    size_t total = sizeof(allocation) + bytes;
    size_t additional = total > previous ? total - previous : 0;
    if (owner != NULL && owner->reserve(owner->context, additional) != 0) return NULL;
    allocation *resized = realloc(block, total);
    if (resized == NULL) {
        if (owner != NULL) owner->release(owner->context, additional);
        return NULL;
    }
    if (owner != NULL && previous > total) owner->release(owner->context, previous - total);
    resized->value.size = total;
    return resized + 1;
}

static char *copy_string(const char *text) {
    size_t length = strlen(text);
    if (length == SIZE_MAX) return NULL;
    char *copy = allocate(length + 1);
    if (copy != NULL) memcpy(copy, text, length + 1);
    return copy;
}

static void discard_global_error(void *context, const char *format, ...) {
    (void)context;
    (void)format;
}

/* Called exactly once by Rust before any private parser operation. This parser's
 * symbols are renamed at compilation, including allocator and initialization state. */
void uqa_sql_xml_initialize(void) {
    (void)xmlMemSetup(release, allocate, resize, copy_string);
    xmlInitParser();
}

uqa_sql_xml_control *uqa_sql_xml_enter(uqa_sql_xml_control *control) {
    /* Allocate per-thread library state outside the operation's lifetime. */
    (void)xmlGetLastError();
    /* A failed context constructor has no local error handler yet. Its status
     * and the admission failure carry the diagnostic; it must not write stderr. */
    xmlSetGenericErrorFunc(NULL, discard_global_error);
    uqa_sql_xml_control *previous = active_control;
    active_control = control;
    return previous;
}

void uqa_sql_xml_leave(uqa_sql_xml_control *previous) {
    xmlResetLastError();
    active_control = previous;
}

int uqa_sql_xml_check(void) {
    return active_control == NULL ? 0 : active_control->reserve(active_control->context, 0);
}
