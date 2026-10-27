/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_SQL_XML_ALLOCATION_H
#define UQA_SQL_XML_ALLOCATION_H

#include <stddef.h>

typedef struct uqa_sql_xml_control {
    void *context;
    int (*reserve)(void *context, size_t bytes);
    void (*release)(void *context, size_t bytes);
} uqa_sql_xml_control;

void uqa_sql_xml_initialize(void);
uqa_sql_xml_control *uqa_sql_xml_enter(uqa_sql_xml_control *control);
void uqa_sql_xml_leave(uqa_sql_xml_control *previous);
int uqa_sql_xml_check(void);

#endif
