-- BW-15-05-03 — the SQLite-mode table shapes the ported SecondaryDatabase
-- face reads.
--
-- crates/db/src/secondary_database.rs ports the reference BaseApp SQLite
-- secondary database (server/baseapp/sqlite_database.cpp): two data tables
-- `tbl_flip` and `tbl_flop` flip-flopped on every commit (flip holds the
-- older write, flop the newer), plus a `tbl_checksum` table carrying the
-- digest of the newer write. The port's reader runs
-- `SELECT sm_dbID, sm_typeID, sm_time, sm_blob FROM <table>` against the
-- current side and probes a table's existence with
-- `SELECT COUNT(*) FROM <table> WHERE 0` — a statement that only compiles
-- when the table exists. This case creates both sides with the port's own
-- DDL shapes and proves both the data query and the probe compile and return
-- against either side, in either flip-then-flop read order.
--
-- Assertion idiom: the assert table has CHECK (v = 0); an assert emits a
-- nonzero row only when a contract is violated, and a held contract emits
-- nothing. The table name is per-case because a backend run shares one
-- database across its cases.

CREATE TABLE tbl_checksum (sm_checksum TEXT);
CREATE TABLE tbl_flip (sm_dbID INTEGER, sm_typeID INTEGER, sm_time INTEGER, sm_blob BLOB);
CREATE TABLE tbl_flop (sm_dbID INTEGER, sm_typeID INTEGER, sm_time INTEGER, sm_blob BLOB);
CREATE TABLE bw_flipflop_assert (v BIGINT NOT NULL, CHECK (v = 0));

-- Existence probes: the port's own idiom compiles against both sides.
SELECT COUNT(*) FROM tbl_flip WHERE 0;
SELECT COUNT(*) FROM tbl_flop WHERE 0;

-- The port's data query shape compiles against both sides too.
SELECT sm_dbID, sm_typeID, sm_time, sm_blob FROM tbl_flip WHERE 0;
SELECT sm_dbID, sm_typeID, sm_time, sm_blob FROM tbl_flop WHERE 0;

-- A flip-then-flop fixture: flip holds the older write, flop the newer one.
INSERT INTO tbl_flip (sm_dbID, sm_typeID, sm_time, sm_blob) VALUES (1001, 1, 500, X'00AA');
INSERT INTO tbl_flop (sm_dbID, sm_typeID, sm_time, sm_blob) VALUES (1001, 1, 900, X'00BB');
INSERT INTO tbl_checksum (sm_checksum) VALUES ('flip-flop-checksum');

INSERT INTO bw_flipflop_assert (v) SELECT 1 FROM tbl_flip
    WHERE sm_dbID <> 1001 OR sm_typeID <> 1 OR sm_time <> 500 OR sm_blob <> X'00AA';
INSERT INTO bw_flipflop_assert (v) SELECT 1 FROM tbl_flop
    WHERE sm_time <> 900 OR sm_blob <> X'00BB';

-- The checksum table round-trips its TEXT digest.
INSERT INTO bw_flipflop_assert (v) SELECT 1 FROM tbl_checksum
    WHERE sm_checksum <> 'flip-flop-checksum';

-- The reader-side invariant: every surviving db id appears on both sides of
-- a committed flip-flop pair (the older and newer write of the same entity).
INSERT INTO bw_flipflop_assert (v)
    SELECT 1 FROM tbl_flip f LEFT JOIN tbl_flop p ON f.sm_dbID = p.sm_dbID
    WHERE p.sm_dbID IS NULL;
INSERT INTO bw_flipflop_assert (v)
    SELECT 1 FROM tbl_flop p LEFT JOIN tbl_flip f ON p.sm_dbID = f.sm_dbID
    WHERE f.sm_dbID IS NULL;
