-- OPT-09-01-01 — the baseline dataset, collected on both backends.
--
-- One dataset, both faces: this file is a common store case, so ci/store.sh
-- runs the very same statements against a real MariaDB and a real
-- PostgreSQL. Row i (0-based, 24 rows) has id 1000+i, name 'ent-<id>',
-- health 100+i, speed (i%4)*0.5 (exact in binary floating point, so the
-- sum compares exactly), alive when i%3 = 0; plus the account mapping's
-- four rows (alpha..delta onto the first four ids). The assertions are
-- the statements themselves: every guard row violates its CHECK unless
-- the count or sum it pins is exact, so the case is green only when the
-- whole dataset landed identically on both engines — the acceptance is
-- that both baselines are collected over this same dataset.

CREATE TABLE tbl_bwbaseline (
    id BIGINT NOT NULL PRIMARY KEY,
    sm_name VARCHAR(64) NOT NULL,
    sm_health BIGINT NOT NULL,
    sm_speed DOUBLE PRECISION NOT NULL,
    sm_alive BOOLEAN NOT NULL
);

INSERT INTO tbl_bwbaseline VALUES (1000, 'ent-1000', 100, 0.0, TRUE);
INSERT INTO tbl_bwbaseline VALUES (1001, 'ent-1001', 101, 0.5, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1002, 'ent-1002', 102, 1.0, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1003, 'ent-1003', 103, 1.5, TRUE);
INSERT INTO tbl_bwbaseline VALUES (1004, 'ent-1004', 104, 0.0, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1005, 'ent-1005', 105, 0.5, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1006, 'ent-1006', 106, 1.0, TRUE);
INSERT INTO tbl_bwbaseline VALUES (1007, 'ent-1007', 107, 1.5, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1008, 'ent-1008', 108, 0.0, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1009, 'ent-1009', 109, 0.5, TRUE);
INSERT INTO tbl_bwbaseline VALUES (1010, 'ent-1010', 110, 1.0, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1011, 'ent-1011', 111, 1.5, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1012, 'ent-1012', 112, 0.0, TRUE);
INSERT INTO tbl_bwbaseline VALUES (1013, 'ent-1013', 113, 0.5, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1014, 'ent-1014', 114, 1.0, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1015, 'ent-1015', 115, 1.5, TRUE);
INSERT INTO tbl_bwbaseline VALUES (1016, 'ent-1016', 116, 0.0, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1017, 'ent-1017', 117, 0.5, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1018, 'ent-1018', 118, 1.0, TRUE);
INSERT INTO tbl_bwbaseline VALUES (1019, 'ent-1019', 119, 1.5, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1020, 'ent-1020', 120, 0.0, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1021, 'ent-1021', 121, 0.5, TRUE);
INSERT INTO tbl_bwbaseline VALUES (1022, 'ent-1022', 122, 1.0, FALSE);
INSERT INTO tbl_bwbaseline VALUES (1023, 'ent-1023', 123, 1.5, FALSE);

CREATE TABLE bigworldlogonmapping (
    logonname VARCHAR(64) NOT NULL PRIMARY KEY,
    password VARCHAR(64) NOT NULL,
    entitytype BIGINT NOT NULL,
    entityid BIGINT NOT NULL
);

INSERT INTO bigworldlogonmapping VALUES ('alpha', 'pw-alpha', 4, 1000);
INSERT INTO bigworldlogonmapping VALUES ('bravo', 'pw-bravo', 4, 1001);
INSERT INTO bigworldlogonmapping VALUES ('charlie', 'pw-charlie', 4, 1002);
INSERT INTO bigworldlogonmapping VALUES ('delta', 'pw-delta', 4, 1003);

-- The guard tables: a row whose value is not the pinned 0 violates the
-- CHECK and the statement fails, which fails the case.
CREATE TABLE bw_check (v BIGINT NOT NULL CHECK (v = 0));
CREATE TABLE bw_check_real (v DOUBLE PRECISION NOT NULL CHECK (v = 0));

-- The entity set: 24 rows, every id distinct, the pattern's pinned sums.
INSERT INTO bw_check SELECT COUNT(*) - 24 FROM tbl_bwbaseline;
INSERT INTO bw_check SELECT COUNT(DISTINCT id) - 24 FROM tbl_bwbaseline;
INSERT INTO bw_check SELECT SUM(sm_health) - 2676 FROM tbl_bwbaseline;
INSERT INTO bw_check SELECT COUNT(*) - 8 FROM tbl_bwbaseline WHERE sm_alive = TRUE;
INSERT INTO bw_check_real SELECT SUM(sm_speed) - 18.0 FROM tbl_bwbaseline;

-- The account mapping: four rows onto four distinct entities, unique
-- log-on names.
INSERT INTO bw_check SELECT COUNT(*) - 4 FROM bigworldlogonmapping;
INSERT INTO bw_check SELECT COUNT(DISTINCT logonname) - 4 FROM bigworldlogonmapping;
INSERT INTO bw_check SELECT COUNT(DISTINCT entityid) - 4 FROM bigworldlogonmapping;

-- The write face: an even-id update lands on 12 rows and moves the sum
-- by exactly 12; a delete leaves 23 rows. Both engines answer with the
-- same affected counts and the same re-read sums.
UPDATE tbl_bwbaseline SET sm_health = sm_health + 1 WHERE id % 2 = 0;
INSERT INTO bw_check SELECT SUM(sm_health) - 2688 FROM tbl_bwbaseline;
DELETE FROM tbl_bwbaseline WHERE id = 1023;
INSERT INTO bw_check SELECT COUNT(*) - 23 FROM tbl_bwbaseline;
INSERT INTO bw_check SELECT SUM(sm_health) - 2565 FROM tbl_bwbaseline;
