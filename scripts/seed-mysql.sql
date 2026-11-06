-- RustDb P2 live test seed (MySQL 5.7).
-- Idempotent: safe to run again via `docker exec -i ... mysql`.
SET NAMES utf8mb4;
DROP DATABASE IF EXISTS rustdb_test;
CREATE DATABASE rustdb_test CHARACTER SET utf8mb4;
USE rustdb_test;

CREATE TABLE channels (
    id   int PRIMARY KEY,
    name varchar(64) NOT NULL
) DEFAULT CHARSET=utf8mb4;
INSERT INTO channels (id, name) VALUES
    (1, '自然搜索'),
    (2, '付费投放'),
    (3, '好友推荐');

CREATE TABLE orders (
    id          int PRIMARY KEY,
    channel_id  int NOT NULL,
    total       decimal(12,2) NOT NULL,
    note        text,
    CONSTRAINT fk_orders_channel FOREIGN KEY (channel_id) REFERENCES channels(id)
) DEFAULT CHARSET=utf8mb4;
INSERT INTO orders (id, channel_id, total, note) VALUES
    (1, 1, 100.00, 'a'),
    (2, 1, 400.00, 'b'),
    (3, 2, 300.00, 'c'),
    (4, 3,  50.00, 'd');

-- Object-tree coverage required by db-mysql/tests/live.rs.
DELIMITER $$
CREATE PROCEDURE p_demo() BEGIN SELECT 1; END$$
CREATE FUNCTION f_double(n int) RETURNS int DETERMINISTIC
BEGIN RETURN n * 2; END$$
DELIMITER ;
CREATE TRIGGER trg_demo BEFORE INSERT ON orders FOR EACH ROW SET @rustdb_probe = 1;

-- Table used by P2 transaction / fetch_page live cases.
CREATE TABLE p2_tx_probe (
    id  int PRIMARY KEY,
    val varchar(64) NOT NULL
) DEFAULT CHARSET=utf8mb4;

-- A wider table so page_size / streaming contract tests have >1 batch.
CREATE TABLE p2_rows (id int PRIMARY KEY, val varchar(64) NOT NULL);
DROP PROCEDURE IF EXISTS seed_p2_rows;
DELIMITER $$
CREATE PROCEDURE seed_p2_rows()
BEGIN
  DECLARE i int DEFAULT 1;
  WHILE i <= 500 DO
    INSERT INTO p2_rows (id, val) VALUES (i, CONCAT('row-', i));
    SET i = i + 1;
  END WHILE;
END$$
DELIMITER ;
CALL seed_p2_rows();
DROP PROCEDURE seed_p2_rows;
