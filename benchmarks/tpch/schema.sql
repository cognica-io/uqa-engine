CREATE TABLE region (
    r_regionkey BIGINT,
    r_name CHAR(25),
    r_comment VARCHAR(152)
);

CREATE TABLE nation (
    n_nationkey BIGINT,
    n_name CHAR(25),
    n_regionkey BIGINT,
    n_comment VARCHAR(152)
);

CREATE TABLE part (
    p_partkey BIGINT,
    p_name VARCHAR(55),
    p_mfgr CHAR(25),
    p_brand CHAR(10),
    p_type VARCHAR(25),
    p_size INTEGER,
    p_container CHAR(10),
    p_retailprice NUMERIC(15, 2),
    p_comment VARCHAR(23)
);

CREATE TABLE supplier (
    s_suppkey BIGINT,
    s_name CHAR(25),
    s_address VARCHAR(40),
    s_nationkey BIGINT,
    s_phone CHAR(15),
    s_acctbal NUMERIC(15, 2),
    s_comment VARCHAR(101)
);

CREATE TABLE partsupp (
    ps_partkey BIGINT,
    ps_suppkey BIGINT,
    ps_availqty INTEGER,
    ps_supplycost NUMERIC(15, 2),
    ps_comment VARCHAR(199)
);

CREATE TABLE customer (
    c_custkey BIGINT,
    c_name VARCHAR(25),
    c_address VARCHAR(40),
    c_nationkey BIGINT,
    c_phone CHAR(15),
    c_acctbal NUMERIC(15, 2),
    c_mktsegment CHAR(10),
    c_comment VARCHAR(117)
);

CREATE TABLE orders (
    o_orderkey BIGINT,
    o_custkey BIGINT,
    o_orderstatus CHAR(1),
    o_totalprice NUMERIC(15, 2),
    o_orderdate DATE,
    o_orderpriority CHAR(15),
    o_clerk CHAR(15),
    o_shippriority INTEGER,
    o_comment VARCHAR(79)
);

CREATE TABLE lineitem (
    l_orderkey BIGINT,
    l_partkey BIGINT,
    l_suppkey BIGINT,
    l_linenumber INTEGER,
    l_quantity NUMERIC(15, 2),
    l_extendedprice NUMERIC(15, 2),
    l_discount NUMERIC(15, 2),
    l_tax NUMERIC(15, 2),
    l_returnflag CHAR(1),
    l_linestatus CHAR(1),
    l_shipdate DATE,
    l_commitdate DATE,
    l_receiptdate DATE,
    l_shipinstruct CHAR(25),
    l_shipmode CHAR(10),
    l_comment VARCHAR(44)
);
