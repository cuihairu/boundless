-- BW-15-05-03 — the MySQL-mode AUTO_INCREMENT contract the ported storage
-- relies on.
--
-- crates/db's MySQL-mode faces assume the reference backend's identity
-- semantics: NULL inserts get assigned ids, explicit-id inserts are accepted
-- alongside them, an explicit id ABOVE the counter advances it, and an
-- explicit id below it does not lower the counter. PostgreSQL behaves
-- differently (a BY DEFAULT identity never advances on explicit inserts),
-- which is why crates/db-postgres carries bump_identity; this case pins the
-- MySQL side of that dialect difference.
--
-- Assertion idiom: bw_assert has CHECK (v = 0), so any row it receives with
-- v <> 0 refuses the insert and fails the case. Every assert therefore emits
-- a nonzero value only when a contract is violated; a held contract emits no
-- row at all. (The harness runs every statement, so a refused insert fails
-- the case — see the run_case contract in ci/store.sh.)

CREATE TABLE bw_ids (
    id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(64) NOT NULL
);
CREATE TABLE bw_assert (v BIGINT NOT NULL, CHECK (v = 0));

-- NULL inserts are assigned 1 and 2.
INSERT INTO bw_ids (id, name) VALUES (NULL, 'first');
INSERT INTO bw_ids (id, name) VALUES (NULL, 'second');
INSERT INTO bw_assert (v) SELECT 1 FROM bw_ids
    WHERE id NOT IN (1, 2);
INSERT INTO bw_assert (v) SELECT 1 FROM bw_ids
    HAVING COUNT(*) <> 2;

-- An explicit id above the counter advances it (MySQL assigns the next id
-- past the largest explicit insert).
INSERT INTO bw_ids (id, name) VALUES (1000, 'explicit');
INSERT INTO bw_ids (id, name) VALUES (NULL, 'after-explicit');
INSERT INTO bw_assert (v) SELECT 1 FROM bw_ids
    WHERE name = 'after-explicit' AND id <> 1001;

-- An explicit id below the counter does not lower it.
INSERT INTO bw_ids (id, name) VALUES (5, 'low-explicit');
INSERT INTO bw_ids (id, name) VALUES (NULL, 'after-low');
INSERT INTO bw_assert (v) SELECT 1 FROM bw_ids
    WHERE name = 'after-low' AND id <> 1002;
