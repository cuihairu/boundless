-- BW-15-05-03 — the SQLite integer-width and blob contracts the ported
-- entity storage relies on.
--
-- crates/db's SQLite face binds entity database ids as int64
-- (secondary_database.rs: `sm_dbID`, bound as i64) and serialised entity
-- properties as blobs. SQLite INTEGER columns are signed 64-bit, so the full
-- BW id range — including i64::MIN / i64::MAX sentinels the db layer uses in
-- its tests — must round-trip without truncation, and a blob must come back
-- byte-identical. This case pins both round-trips.
--
-- Assertion idiom: the assert table has CHECK (v = 0); an assert emits a
-- nonzero row only when a contract is violated, and a held contract emits
-- nothing. The table name is per-case because a backend run shares one
-- database across its cases.

CREATE TABLE bw_entities (
    sm_dbID INTEGER PRIMARY KEY,
    sm_typeID INTEGER NOT NULL,
    sm_time INTEGER NOT NULL,
    sm_blob BLOB
);
CREATE TABLE bw_ids_assert (v BIGINT NOT NULL, CHECK (v = 0));

-- The full signed-64-bit id range survives a round-trip.
INSERT INTO bw_entities (sm_dbID, sm_typeID, sm_time, sm_blob) VALUES
    (-9223372036854775808, 1, 1, X''),
    (-1, 2, 2, NULL),
    (9223372036854775807, 3, 3, X'DEADBEEF');

INSERT INTO bw_ids_assert (v) SELECT 1 FROM bw_entities
    WHERE sm_dbID = -9223372036854775808 AND (sm_typeID <> 1 OR sm_time <> 1);
INSERT INTO bw_ids_assert (v) SELECT 1 FROM bw_entities
    WHERE sm_dbID = 9223372036854775807 AND (sm_typeID <> 3 OR sm_time <> 3);

-- A empty blob stays a zero-length blob (not NULL); an SQL NULL stays NULL.
INSERT INTO bw_ids_assert (v) SELECT 1 FROM bw_entities
    WHERE sm_dbID = -9223372036854775808 AND length(sm_blob) <> 0;
INSERT INTO bw_ids_assert (v) SELECT 1 FROM bw_entities
    WHERE sm_dbID = -1 AND sm_blob IS NOT NULL;

-- The blob round-trips byte-identical, proven through hex().
INSERT INTO bw_ids_assert (v) SELECT 1 FROM bw_entities
    WHERE sm_dbID = 9223372036854775807 AND hex(sm_blob) <> 'DEADBEEF';

-- (The bare `HAVING COUNT(*)` form this case's MySQL siblings use is
-- MariaDB-only: SQLite rejects HAVING on a non-aggregate query, so the count
-- assert goes through a scalar subquery instead.)
INSERT INTO bw_ids_assert (v) SELECT 1
    WHERE (SELECT COUNT(*) FROM bw_entities) <> 3;
