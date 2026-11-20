-- BW-22-02-02 — the storage contract of the id vocabulary that already exists.
--
-- crates/foundation/src/ids.rs fixes DatabaseId = i64 and
-- PENDING_DATABASE_ID = -1. Nothing in this tree reads a database yet, so this
-- is the minimum a backend must satisfy before BW-09-02 starts mapping tables:
-- the id column must be a signed 64-bit integer and must accept the sentinel
-- and both ends of the i64 range.
--
-- The assertions are the statements themselves. An engine that narrowed the
-- column rejects one of these inserts, so a case is only green when the whole
-- i64 domain round-trips.

CREATE TABLE bw_id_contract (id BIGINT NOT NULL PRIMARY KEY);

-- PENDING_DATABASE_ID
INSERT INTO bw_id_contract VALUES (-1);
-- i64::MIN
INSERT INTO bw_id_contract VALUES (-9223372036854775808);
-- the assigned-id boundary
INSERT INTO bw_id_contract VALUES (0);
-- i64::MAX
INSERT INTO bw_id_contract VALUES (9223372036854775807);