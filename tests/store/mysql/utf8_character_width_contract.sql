-- BW-15-05-03 — the MySQL-mode utf8 width contract the ported charset faces
-- assume.
--
-- crates/db's charset module (utf8_char_offsets / utf8_byte_length_for /
-- utf8_truncate_to_chars) models the reference's per-charset width table, and
-- the MySQL column mapping it feeds is CHARACTER-counted for VARCHAR: the
-- width limit is characters, not bytes, so one 4-byte utf8mb4 character fits
-- a VARCHAR(3). A backend that counted bytes instead would refuse it and this
-- case would go red. (The refusal half — an oversized literal is rejected in
-- strict mode — is the backend's own behaviour; it cannot appear in a case
-- whose contract is "every statement succeeds", so it stays with the Rust
-- map_db_error tests.)
--
-- Assertion idiom: the assert table has CHECK (v = 0); an assert emits a
-- nonzero row only when a contract is violated, and a held contract emits
-- nothing. The table name is per-case because a backend run shares one
-- database across its cases.

CREATE TABLE bw_width (s VARCHAR(3) NOT NULL) DEFAULT CHARSET utf8mb4;
CREATE TABLE bw_width_assert (v BIGINT NOT NULL, CHECK (v = 0));

-- Three characters fill the column exactly.
INSERT INTO bw_width (s) VALUES ('abc');

-- One 4-byte utf8mb4 character is ONE character, so it fits VARCHAR(3).
INSERT INTO bw_width (s) VALUES ('😀');
INSERT INTO bw_width_assert (v) SELECT 1 FROM bw_width
    WHERE CHAR_LENGTH(s) = 2;

-- Two characters (one plain, one 4-byte) also fit, and CHAR_LENGTH counts
-- characters, not the 5 bytes they occupy.
INSERT INTO bw_width (s) VALUES ('a😀');
INSERT INTO bw_width_assert (v) SELECT 1 FROM bw_width
    WHERE s = 'a😀' AND (CHAR_LENGTH(s) <> 2 OR LENGTH(s) <> 5);

INSERT INTO bw_width_assert (v) SELECT 1 FROM bw_width
    HAVING COUNT(*) <> 3;
