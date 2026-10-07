-- Historical table definitions from w5_storage/v5.sql (no profile/data rewrite).
CREATE TABLE source_indexes (
            owner_id TEXT PRIMARY KEY, generation INTEGER NOT NULL,
            processing_version TEXT NOT NULL, source_revision INTEGER NOT NULL);
CREATE TABLE source_segments (
            id INTEGER PRIMARY KEY, owner_id TEXT NOT NULL, source_id TEXT NOT NULL,
            source_version INTEGER NOT NULL, generation INTEGER NOT NULL, task_id TEXT,
            received_sequence INTEGER NOT NULL, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL,
            text TEXT NOT NULL, index_text TEXT NOT NULL,
            UNIQUE(owner_id, generation, source_id, byte_start));
CREATE VIRTUAL TABLE source_fts USING fts5(index_text,
            content='source_segments', content_rowid='id', tokenize='unicode61 remove_diacritics 0');
CREATE INDEX source_segments_scope ON source_segments(owner_id, generation, task_id);
CREATE TRIGGER source_segments_insert AFTER INSERT ON source_segments BEGIN
            INSERT INTO source_fts(rowid, index_text) VALUES (new.id, new.index_text);
         END;
CREATE TRIGGER source_segments_delete AFTER DELETE ON source_segments BEGIN
            INSERT INTO source_fts(source_fts, rowid, index_text) VALUES ('delete', old.id, old.index_text);
         END;
CREATE TRIGGER source_segments_update AFTER UPDATE ON source_segments BEGIN
            INSERT INTO source_fts(source_fts, rowid, index_text) VALUES ('delete', old.id, old.index_text);
            INSERT INTO source_fts(rowid, index_text) VALUES (new.id, new.index_text);
         END;
