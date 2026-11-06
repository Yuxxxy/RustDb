-- RustDb P2 live test seed (PostgreSQL 15).
-- Idempotent: safe to run again via `docker exec -i ... psql`.
DROP SCHEMA IF EXISTS shop CASCADE;
CREATE SCHEMA shop;

CREATE TABLE shop.channels (
    id   integer PRIMARY KEY,
    name text NOT NULL
);
INSERT INTO shop.channels (id, name) VALUES
    (1, '自然搜索'),
    (2, '付费投放'),
    (3, '好友推荐');

CREATE TABLE shop.orders (
    id          integer PRIMARY KEY,
    channel_id  integer NOT NULL REFERENCES shop.channels(id),
    total       numeric(12,2) NOT NULL,
    status      text NOT NULL DEFAULT 'paid',
    note        text
);
INSERT INTO shop.orders (id, channel_id, total, status, note) VALUES
    (1, 1, 100.00, 'paid',     'a'),
    (2, 1, 400.00, 'paid',     'b'),
    (3, 2, 300.00, 'refunded', 'c'),
    (4, 3,  50.00, 'paid',     'd');

-- Object-tree coverage required by db-postgres/tests/live.rs.
CREATE MATERIALIZED VIEW shop.mv_channels AS SELECT * FROM shop.channels;
CREATE SEQUENCE shop.seq_order_id START 100;
CREATE FUNCTION shop.f_double(n integer) RETURNS integer
    LANGUAGE sql IMMUTABLE AS $$ SELECT n * 2 $$;
CREATE PROCEDURE shop.p_touch()
    LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;
CREATE TYPE shop.mood AS ENUM ('happy', 'sad');

-- Table used by P2 transaction / fetch_page live cases.
CREATE TABLE shop.p2_tx_probe (
    id  integer PRIMARY KEY,
    val text NOT NULL
);

-- A wider table so page_size / streaming contract tests have >1 batch.
CREATE TABLE shop.p2_rows AS
    SELECT g AS id, 'row-' || g AS val
    FROM generate_series(1, 500) AS g;
