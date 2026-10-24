#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture XML input and output using an explicit disposable PostgreSQL 18 connection."""

import ctypes as c
import ctypes.util
import json
import os


INPUTS = [
    "", " ", "\n<root/>", "\n\n<root/>", "plain text", "<root/>", "<root/>tail", "<a/><b/>",
    "<!-- comment -->", "<?instruction value?>", "<![CDATA[text]]>",
    "<a>&amp;&lt;&gt;&quot;&apos;</a>", "<a>한글 日本語 é</a>",
    "<a>\r\ntext\rmore</a>", "<a/ >", "<a>", "<a></b>",
    "<a x='1' x='2'/>", "<a>&missing;</a>", "<a>&#0;</a>",
    "<a>&#x1F642;</a>", "<a>\u0001</a>", "<a>]]></a>",
    "<ns:root/>", "<a xmlns:xml='wrong'/>",
    "<a xmlns:p='same' xmlns:q='same' p:x='1' q:x='2'/>",
    "<?xml version='1.0'?><root/>", "<?xml version='1.0'?>\n<root/>",
    "<?xml version='1.1'?><root/>", "<?xml version='1.2'?><root/>",
    "<?xml version='bad'?><root/>", "<?xml version=''?><root/>",
    "<?xml version='1.0' encoding='UTF-8'?><root/>",
    "<?xml version='1.0' encoding='ISO-8859-1'?><a>é</a>",
    "<?xml version='1.0' encoding='unknown'?><root/>",
    "<?xml version='1.0' encoding=''?><root/>",
    "<?xml version='1.0' standalone='yes'?><root/>",
    "<?xml version='1.0' standalone='no'?><root/>",
    "<?xml version='1.0' standalone='maybe'?><root/>",
    "<?xml version='1.0' encoding='UTF-8' standalone='yes'?><root/>",
    "<?xml version='1.0' standalone='yes' encoding='UTF-8'?><root/>",
    "<?xml version='1.0'encoding='UTF-8'?><root/>",
    "<?xml?><root/>", "<?xml encoding='UTF-8'?><root/>",
    " <?xml version='1.0'?><root/>", "<?XML version='1.0'?><root/>",
    "<?xml-stylesheet href='test'?><root/>", "<?xmlé?><root/>",
    "\ufeff<root/>", "\ufeff<?xml version='1.0'?><root/>",
    "<!DOCTYPE a><a/>", "<!-- before --><!DOCTYPE a><a/>",
    "<?before?><!DOCTYPE a><a/>", "<!DOCTYPE a><a/><b/>",
    "<!DOCTYPE a [<!ENTITY x 'value'>]><a>&x;</a>",
    "<!DOCTYPE a [<!ATTLIST a x CDATA 'default'>]><a/>",
    "<!DOCTYPE a SYSTEM 'file:///uqa-xml-reference-does-not-exist'><a/>",
    "<!DOCTYPE a [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><a>&x;</a>",
    "<!DOCTYPE a [<!ENTITY x SYSTEM 'https://example.invalid/entity'>]><a>&x;</a>",
    "<?xml version='1.0'?>text<a/>", "<?xml version='1.0'?>",
]

STATEMENTS = [
    "SELECT oid, typname, typcategory, typcollation, typelem, typarray FROM pg_type WHERE oid IN (142, 143) ORDER BY oid",
    "SELECT NULL::xml AS absent, '<a/>'::xml AS value, ARRAY['<a/>'::xml, NULL] AS items",
    "SELECT '<a/>'::xml = '<a/>'::xml",
    "SELECT '<a/>'::xml ORDER BY 1",
    "SELECT DISTINCT '<a/>'::xml",
    "SELECT 1::xml",
    "SET xmloption TO content",
    "CREATE TEMP TABLE xml_values (id integer, value xml, items xml[])",
    "INSERT INTO xml_values VALUES (1, 'plain text', ARRAY['<a/>'::xml, 'text'::xml])",
    "PREPARE xml_literal AS SELECT 'text'::xml AS value",
    "PREPARE xml_runtime(text) AS SELECT $1::xml AS value",
    "SET xmloption TO document",
    "EXECUTE xml_literal",
    "EXECUTE xml_runtime('text')",
    "INSERT INTO xml_values SELECT 2, value, items FROM xml_values WHERE id = 1",
    "SELECT id, value, items FROM xml_values ORDER BY id",
    "SELECT '{<a/>,text}'::xml[]",
    "BEGIN",
    "SAVEPOINT keep_xml",
    "SELECT 'text'::xml",
    "SELECT '<a/>'::xml",
    "ROLLBACK TO keep_xml",
    "SELECT '<a/>'::xml",
    "ROLLBACK",
    "SET xmloption TO content",
    "SELECT '<?xml version=\"1.0\" encoding=\"UTF-8\"?><a/>'::xml::text",
    "SELECT ARRAY['<?xml version=\"1.0\"?><a/>'::xml, NULL]::text",
]


def capture(connection_string):
    pq = c.CDLL(ctypes.util.find_library("pq"))

    def function(name, result, *arguments):
        callback = getattr(pq, name)
        callback.restype = result
        callback.argtypes = arguments
        return callback

    pointer, string, integer = c.c_void_p, c.c_char_p, c.c_int
    connect = function("PQconnectdb", pointer, string)
    connection_status = function("PQstatus", integer, pointer)
    connection_error = function("PQerrorMessage", string, pointer)
    finish = function("PQfinish", None, pointer)
    clear = function("PQclear", None, pointer)
    query = function("PQexec", pointer, pointer, string)
    parameters = function("PQexecParams", pointer, pointer, string, integer,
                          c.POINTER(c.c_uint), c.POINTER(string),
                          c.POINTER(integer), c.POINTER(integer), integer)
    status = function("PQresultStatus", integer, pointer)
    error = function("PQresultErrorField", string, pointer, integer)
    value = function("PQgetvalue", string, pointer, integer, integer)
    field_type = function("PQftype", c.c_uint, pointer, integer)
    rows = function("PQntuples", integer, pointer)
    columns = function("PQnfields", integer, pointer)
    is_null = function("PQgetisnull", integer, pointer, integer, integer)
    transaction_status = function("PQtransactionStatus", integer, pointer)
    receiver_type = c.CFUNCTYPE(None, pointer, pointer)
    notice_receiver = function("PQsetNoticeReceiver", pointer, pointer, receiver_type, pointer)
    notices = []

    @receiver_type
    def receive_notice(_context, result):
        notices.append({key: error(result, ord(code)).decode()
                        for key, code in (("sqlstate", "C"), ("message", "M"))})

    connection = connect(connection_string.encode())
    if not connection or connection_status(connection) != 0:
        raise RuntimeError(connection_error(connection).decode())
    notice_receiver(connection, receive_notice, None)

    def command(sql):
        result = query(connection, sql.encode())
        try:
            if status(result) not in (1, 2):
                raise RuntimeError(error(result, ord("M")).decode())
            return value(result, 0, 0).decode() if status(result) == 2 else None
        finally:
            clear(result)

    try:
        version = command("SELECT version()")
        if not version.startswith("PostgreSQL 18."):
            raise RuntimeError(version)
        cases = []
        for mode in ("content", "document"):
            command(f"SET xmloption TO {mode}")
            for text in INPUTS:
                notices.clear()
                result = parameters(connection,
                    b"SELECT $1::xml AS value, ($1::xml)::text AS stored", 1,
                    None, (string * 1)(text.encode()), None, None, 0)
                try:
                    observation = {"mode": mode, "input": text, "notices": list(notices)}
                    if status(result) == 2:
                        observation.update(output=value(result, 0, 0).decode(),
                                           stored=value(result, 0, 1).decode(),
                                           oid=field_type(result, 0))
                    else:
                        for key, code in (("sqlstate", "C"), ("message", "M"), ("detail", "D")):
                            field = error(result, ord(code))
                            observation[key] = field.decode() if field else None
                    cases.append(observation)
                finally:
                    clear(result)
        statements = []
        for sql in STATEMENTS:
            notices.clear()
            result = query(connection, sql.encode())
            try:
                observation = {"sql": sql, "transaction_status": transaction_status(connection), "notices": list(notices)}
                if status(result) in (1, 2):
                    observation["types"] = [field_type(result, i) for i in range(columns(result))]
                    observation["rows"] = [
                        [None if is_null(result, row, column) else value(result, row, column).decode()
                         for column in range(columns(result))]
                        for row in range(rows(result))
                    ]
                else:
                    observation["sqlstate"] = error(result, ord("C")).decode()
                    observation["message"] = error(result, ord("M")).decode()
                statements.append(observation)
            finally:
                clear(result)
        return {"version": version, "libxml2": os.environ["PG_XML_LIBXML_VERSION"],
                "cases": cases, "statements": statements}
    finally:
        finish(connection)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_XML_CONNECTION"]), indent=2, ensure_ascii=False))
