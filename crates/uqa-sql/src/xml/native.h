/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_SQL_XML_NATIVE_H
#define UQA_SQL_XML_NATIVE_H

#include <stddef.h>

/* Private static-link interface; these functions are not C binding exports. */
typedef struct uqa_sql_xml_notice uqa_sql_xml_notice;
int uqa_sql_xml_validate(const char *input, size_t length, int document,
                         const char *version, size_t version_length,
                         char **detail, uqa_sql_xml_notice **notices);
void uqa_sql_xml_free_detail(char *detail);
int uqa_sql_xml_name_character(unsigned int character);
const char *uqa_sql_xml_notice_message(const uqa_sql_xml_notice *notice, int *level);
const uqa_sql_xml_notice *uqa_sql_xml_notice_next(const uqa_sql_xml_notice *notice);
void uqa_sql_xml_notices_free(uqa_sql_xml_notice *notices);

#endif
