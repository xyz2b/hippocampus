//! Rebuildable, owner-scoped source projection. This is not the public recall policy.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

use crate::*;

pub(crate) const VERSION: &str = "cjk-position-v1";
const SEGMENT_CHARS: usize = 768;
const OVERLAP_CHARS: usize = 128;

pub(crate) fn create_schema(db: &Connection) -> PersistenceResult<()> {
    db.execute_batch(
        "CREATE TABLE source_indexes (
            owner_id TEXT PRIMARY KEY, generation INTEGER NOT NULL,
            processing_version TEXT NOT NULL, source_revision INTEGER NOT NULL);
         CREATE TABLE source_segments (
            id INTEGER PRIMARY KEY, owner_id TEXT NOT NULL, source_id TEXT NOT NULL,
            source_version INTEGER NOT NULL, generation INTEGER NOT NULL, task_id TEXT,
            received_sequence INTEGER NOT NULL, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL,
            text TEXT NOT NULL, index_text TEXT NOT NULL,
            UNIQUE(owner_id, generation, source_id, byte_start));
         CREATE INDEX source_segments_scope ON source_segments(owner_id, generation, task_id);
         CREATE VIRTUAL TABLE source_fts USING fts5(index_text,
            content='source_segments', content_rowid='id', tokenize='unicode61 remove_diacritics 0');
         CREATE TRIGGER source_segments_insert AFTER INSERT ON source_segments BEGIN
            INSERT INTO source_fts(rowid, index_text) VALUES (new.id, new.index_text);
         END;
         CREATE TRIGGER source_segments_delete AFTER DELETE ON source_segments BEGIN
            INSERT INTO source_fts(source_fts, rowid, index_text) VALUES ('delete', old.id, old.index_text);
         END;
         CREATE TRIGGER source_segments_update AFTER UPDATE ON source_segments BEGIN
            INSERT INTO source_fts(source_fts, rowid, index_text) VALUES ('delete', old.id, old.index_text);
            INSERT INTO source_fts(rowid, index_text) VALUES (new.id, new.index_text);
         END;",
    )?;
    Ok(())
}

pub(crate) fn metadata(
    db: &Connection,
    owner: &UserId,
) -> PersistenceResult<Option<(u64, String, u64)>> {
    let record: Option<(i64,String,i64)> = db.query_row(
        "SELECT generation, processing_version, source_revision FROM source_indexes WHERE owner_id=?1",
        [&owner.0], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    record
        .map(|(generation, version, revision)| {
            Ok((generation.try_into()?, version, revision.try_into()?))
        })
        .transpose()
}

pub(crate) fn body(source: &SourceRecord) -> Option<&str> {
    let request = source.request.as_ref()?;
    if request["capture_source"] != true {
        return None;
    }
    request["text"].as_str()
}

pub(crate) fn task(source: &SourceRecord) -> Option<&str> {
    source.request.as_ref().and_then(|r| r["task_id"].as_str())
}

fn signature(source: &SourceRecord) -> Value {
    json!([
        source.id,
        source.session_id,
        source.turn_id,
        source.observed_at,
        source.received_sequence,
        body(source),
        task(source)
    ])
}

fn han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af)
}

// Hex-encoded tokens keep FTS operators/normalization out of user-controlled input.
// ponytail: Chinese character positions match literal phrases, not linguistic synonyms;
// replace this versioned processor only if measured word-segmentation quality warrants it.
pub(crate) fn tokens(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut word = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if han(c) || !c.is_alphanumeric() {
            if !word.is_empty() {
                result.push(std::mem::take(&mut word));
            }
            if han(c) {
                result.push(format!("c{:x}", c as u32));
            }
        } else {
            if word.is_empty() {
                word.push('w');
            }
            use std::fmt::Write;
            write!(word, "{:06x}", c as u32).unwrap();
        }
    }
    if !word.is_empty() {
        result.push(word);
    }
    result
}

pub(crate) fn segments(text: &str) -> Vec<(usize, usize)> {
    let offsets: Vec<_> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let count = offsets.len() - 1;
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < count {
        let end = (start + SEGMENT_CHARS).min(count);
        ranges.push((offsets[start], offsets[end]));
        if end == count {
            break;
        }
        start = end - OVERLAP_CHARS;
    }
    ranges
}

fn index_tokens(text: &str, start: usize, end: usize) -> Vec<String> {
    let word = |c: char| c.is_alphanumeric() && !han(c);
    let mut words = tokens(&text[start..end]);
    // Fixed-size chunks must not turn half of an English/number token into a new word.
    if start > 0
        && text[..start].chars().next_back().is_some_and(word)
        && text[start..].chars().next().is_some_and(word)
        && !words.is_empty()
    {
        words.remove(0);
    }
    if end < text.len()
        && text[..end].chars().next_back().is_some_and(word)
        && text[end..].chars().next().is_some_and(word)
    {
        words.pop();
    }
    words
}

fn insert_source(
    db: &Connection,
    owner: &UserId,
    source: &SourceRecord,
    generation: u64,
    version: u64,
) -> PersistenceResult<()> {
    let Some(text) = body(source) else {
        return Ok(());
    };
    if text.chars().count() > 24_000
        || task(source).is_some_and(|t| t.trim().is_empty() || t.chars().count() > 200)
    {
        return Err("source exceeds indexed text/task bounds".into());
    }
    for (start, end) in segments(text) {
        let part = &text[start..end];
        let words = index_tokens(text, start, end);
        db.execute("INSERT INTO source_segments(owner_id,source_id,source_version,generation,task_id,received_sequence,byte_start,byte_end,text,index_text)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![owner.0,source.id,i64::try_from(version)?,i64::try_from(generation)?,task(source),i64::try_from(source.received_sequence)?,i64::try_from(start)?,i64::try_from(end)?,part,words.join(" ")])?;
        // Verify a ready signal against the very same owner and newly written row, without polling.
        if let Some(first) = words.first() {
            let visible: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM source_fts JOIN source_segments s ON s.id=source_fts.rowid
                 WHERE source_fts MATCH ?1 AND s.owner_id=?2 AND s.id=?3)",
                params![format!("\"{first}\""), owner.0, db.last_insert_rowid()], |r| r.get(0))?;
            if !visible {
                return Err("source index read-after-write failed".into());
            }
        }
    }
    Ok(())
}

fn receipt(source: &mut SourceRecord, generation: u64, revision: u64, version: u64) {
    source.index = json!({"status":if source.request.is_none() {"deleted"} else if body(source).is_some() {"ready"} else {"not_requested"},
        "processing_version":VERSION,"generation":generation,"source_revision":revision,"source_version":version});
}

fn rebuild(
    db: &Connection,
    owner: &UserId,
    state: &mut PersistedMemoryState,
) -> PersistenceResult<()> {
    let generation = metadata(db, owner)?
        .map_or(0, |m| m.0)
        .checked_add(1)
        .ok_or("index generation overflow")?;
    let mut ids = BTreeSet::new();
    for source in state.sources.values_mut() {
        if !ids.insert(source.id.clone()) {
            return Err("duplicate source ID".into());
        }
        let version = source.index["source_version"].as_u64().unwrap_or(1);
        insert_source(db, owner, source, generation, version)?;
        receipt(source, generation, state.source_revision, version);
    }
    // Build and validate the new generation before switching; readers see either complete snapshot.
    db.execute(
        "INSERT INTO source_indexes VALUES (?1,?2,?3,?4)
        ON CONFLICT(owner_id) DO UPDATE SET generation=excluded.generation,
        processing_version=excluded.processing_version,source_revision=excluded.source_revision",
        params![
            owner.0,
            i64::try_from(generation)?,
            VERSION,
            i64::try_from(state.source_revision)?
        ],
    )?;
    db.execute(
        "DELETE FROM source_segments WHERE owner_id=?1 AND generation<>?2",
        params![owner.0, i64::try_from(generation)?],
    )?;
    Ok(())
}

pub(crate) fn initialize(
    db: &Connection,
    owner: &UserId,
    state: &mut PersistedMemoryState,
) -> PersistenceResult<bool> {
    if metadata(db, owner)?.is_none() {
        rebuild(db, owner, state)?;
        return Ok(true);
    }
    Ok(false)
}

pub(crate) fn synchronize(
    db: &Connection,
    owner: &UserId,
    before: &BTreeMap<String, SourceRecord>,
    revision: u64,
    state: &mut PersistedMemoryState,
) -> PersistenceResult<()> {
    for (key, old) in before {
        let new = state
            .sources
            .get(key)
            .ok_or("source tombstone must be retained")?;
        if old.request.is_none() && new.request.is_some() {
            return Err("deleted source cannot be restored".into());
        }
        if old.id != new.id || old.session_id != new.session_id || old.turn_id != new.turn_id {
            return Err("source identity cannot be changed".into());
        }
    }
    let (generation, processor, indexed_revision) =
        metadata(db, owner)?.unwrap_or((0, "not_built".into(), 0));
    let changed = state.sources.iter().any(|(key, s)| {
        before
            .get(key)
            .is_none_or(|old| signature(old) != signature(s))
    });
    state.source_revision = if changed {
        revision.checked_add(1).ok_or("source revision overflow")?
    } else {
        revision
    };
    if processor != VERSION || indexed_revision != revision {
        // An old projection may block new indexing, but cannot block privacy deletion
        // or unrelated fact/receipt updates. Keep remaining coverage explicitly stale.
        for (key, source) in &mut state.sources {
            let old = before.get(key);
            let changed = old.is_none_or(|old| signature(old) != signature(source));
            if changed && body(source).is_some() {
                return Err("source index version mismatch; rebuild explicitly".into());
            }
            if changed {
                db.execute(
                    "DELETE FROM source_segments WHERE owner_id=?1 AND source_id=?2",
                    params![owner.0, source.id],
                )?;
                let version = old
                    .and_then(|s| s.index["source_version"].as_u64())
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("source version overflow")?;
                source.index = json!({"status":if source.request.is_none(){"deleted"}else{"not_requested"},
                    "processing_version":processor,"generation":generation,"source_revision":state.source_revision,"source_version":version});
            } else if let Some(old) = old {
                source.index = old.index.clone();
            }
        }
        return Ok(());
    }
    let mut ids = BTreeSet::new();
    for (key, source) in &mut state.sources {
        if !ids.insert(source.id.clone()) {
            return Err("duplicate source ID".into());
        }
        let old = before.get(key);
        let changed = old.is_none_or(|old| signature(old) != signature(source));
        let mut version = old
            .and_then(|s| s.index["source_version"].as_u64())
            .unwrap_or(0);
        if changed {
            version = version.checked_add(1).ok_or("source version overflow")?;
            db.execute(
                "DELETE FROM source_segments WHERE owner_id=?1 AND source_id=?2",
                params![owner.0, source.id],
            )?;
            insert_source(db, owner, source, generation, version)?;
        }
        receipt(source, generation, state.source_revision, version);
    }
    db.execute(
        "UPDATE source_indexes SET source_revision=?2 WHERE owner_id=?1",
        params![owner.0, i64::try_from(state.source_revision)?],
    )?;
    Ok(())
}

fn query_expression(
    query: &str,
    state: &PersistedMemoryState,
    owner: &UserId,
) -> PersistenceResult<(String, Vec<String>)> {
    if query.chars().count() > 512 {
        return Err("query exceeds 512 characters".into());
    }
    let groups: Vec<_> = query.split_whitespace().collect();
    if groups.len() > 16 || groups.iter().any(|g| g.chars().count() > 128) {
        return Err("query exceeds term bounds".into());
    }
    let entities = state.entities.list_by_owner(owner);
    let mut phrases = BTreeSet::new();
    for group in groups {
        let normalized = tokens(group);
        if normalized.is_empty() {
            continue;
        }
        phrases.insert(normalized.join(" "));
        for entity in &entities {
            let names = std::iter::once(&entity.canonical_name).chain(&entity.aliases);
            if names.clone().any(|name| tokens(name) == normalized) {
                for name in names {
                    let expansion = tokens(name);
                    if !expansion.is_empty() {
                        phrases.insert(expansion.join(" "));
                    }
                }
            }
        }
    }
    if phrases.is_empty() {
        return Err("query contains no searchable text".into());
    }
    if phrases.len() > 32
        || phrases
            .iter()
            .map(|p| p.split_whitespace().count())
            .sum::<usize>()
            > 128
        || phrases.iter().map(String::len).sum::<usize>() > 8192
    {
        return Err("query/alias expansion exceeds term bounds".into());
    }
    let phrases: Vec<_> = phrases.into_iter().collect();
    let expression = phrases
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    Ok((expression, phrases))
}

/// Literal name hints only: matching a registered name does not resolve the query's subject.
pub(crate) fn entity_hints(query: &str, entities: &[Entity]) -> BTreeMap<EntityId, Vec<String>> {
    let words = format!(" {} ", tokens(query).join(" "));
    entities
        .iter()
        .filter_map(|entity| {
            let names = std::iter::once(&entity.canonical_name).chain(&entity.aliases);
            let matched = names.clone().any(|name| {
                let phrase = tokens(name).join(" ");
                name.chars().count() >= 2
                    && !phrase.is_empty()
                    && words.contains(&format!(" {phrase} "))
            });
            if !matched {
                return None;
            }
            let names: BTreeSet<_> = names
                .map(|name| tokens(name).join(" "))
                .filter(|name| !name.is_empty())
                .collect();
            Some((entity.id.clone(), names.into_iter().collect()))
        })
        .collect()
}

/// Natural queries share the index tokenizer; adjacent Han pairs avoid single-character noise.
// ponytail: literal bigrams plus registered aliases, no synonym inference; measure gaps before adding a planner.
pub(crate) fn natural_phrases(query: &str, entities: &[Entity]) -> Vec<String> {
    let mut phrases = BTreeSet::new();
    for group in query.split(|c: char| !c.is_alphanumeric()) {
        let words = tokens(group);
        for (i, word) in words.iter().enumerate() {
            if word.starts_with('w') {
                phrases.insert(word.clone());
            } else if let Some(next) = words.get(i + 1).filter(|w| w.starts_with('c')) {
                phrases.insert(format!("{word} {next}"));
            } else if words.len() == 1 {
                phrases.insert(word.clone());
            }
        }
    }
    for names in entity_hints(query, entities).into_values() {
        phrases.extend(names);
    }
    phrases.into_iter().collect()
}

pub(crate) fn matches_phrases(text: &str, phrases: &[String]) -> bool {
    let words = format!(" {} ", tokens(text).join(" "));
    phrases.iter().any(|p| words.contains(&format!(" {p} ")))
}

fn memory_links(
    state: &PersistedMemoryState,
    owner: &UserId,
    source: &SourceRecord,
    start: usize,
    end: usize,
    task_id: Option<&str>,
    limit: Option<usize>,
) -> PersistenceResult<(Vec<Value>, bool)> {
    let memories = state.repository.list_by_owner(owner)?;
    let allowed = |m: &Memory| {
        m.status != MemoryStatus::Deleted
            && task_id.is_none_or(|task| {
                m.scope
                    .conditions
                    .iter()
                    .all(|c| !matches!(c,ScopeCondition::Task(t) if t.0!=task))
            })
    };
    let visible_ids: BTreeSet<_> = memories
        .iter()
        .filter(|m| allowed(m))
        .map(|m| m.id)
        .collect();
    let text = body(source).unwrap_or("");
    let w5 = crate::commit::W5View::of(state);
    let mut links = Vec::new();
    let mut uncertain = false;
    for memory in memories.iter().filter(|m| allowed(m)) {
        let evidence: Vec<_> = memory
            .evidence
            .iter()
            .filter(|e| e.session_id.0 == source.session_id && e.turn_id.0 == source.turn_id)
            .collect();
        if evidence.is_empty() {
            continue;
        }
        let mut exact = None;
        let mut ambiguous = false;
        for e in evidence {
            if e.quote.is_empty() {
                ambiguous = true;
                continue;
            }
            let matches: Vec<_> = text.match_indices(&e.quote).collect();
            if matches.len() != 1 {
                ambiguous = true;
                continue;
            }
            let (position, quote) = matches[0];
            if position >= start && position + quote.len() <= end {
                exact = Some((position, position + quote.len()));
            } else if position < end && position + quote.len() > start {
                ambiguous = true;
            }
        }
        if exact.is_none() && !ambiguous {
            continue;
        }
        uncertain |= exact.is_none();
        if limit.is_some_and(|limit| links.len() == limit) {
            uncertain = true;
            break;
        }
        let mut link = json!({"memory_id":memory.id,"status":memory.status,"temporal":memory.temporal,
            "supersede_reason":memory.supersede_reason,
            "supersedes":memory.supersedes.filter(|id|visible_ids.contains(id)),
            "superseded_by":memory.superseded_by.filter(|id|visible_ids.contains(id)),
            "association":if exact.is_some(){"exact_quote"}else{"same_turn_uncertain"},
            "quote_byte_range":exact});
        w5.attach(&mut link, memory.id);
        links.push(link);
    }
    uncertain |= links.is_empty();
    Ok((links, uncertain))
}

fn projection_complete(
    db: &Connection,
    owner: &UserId,
    state: &PersistedMemoryState,
    generation: u64,
    revision: u64,
    task_id: Option<&str>,
) -> PersistenceResult<bool> {
    // ponytail: compare this owner's bounded snapshot projection on reads; normalize source rows
    // and use per-source revision constraints if full-profile read cost becomes material.
    let sources: BTreeMap<_, _> = state
        .sources
        .values()
        .filter(|s| task_id.is_none_or(|t| task(s) == Some(t)))
        .map(|s| (s.id.as_str(), s))
        .collect();
    let mut expected = BTreeSet::new();
    for source in sources.values().filter(|s| body(s).is_some()) {
        if source.index["status"] != "ready"
            || source.index["processing_version"] != VERSION
            || source.index["generation"] != generation
            || source.index["source_revision"] != revision
        {
            return Ok(false);
        }
        for (start, end) in segments(body(source).unwrap()) {
            expected.insert((source.id.as_str(), start, end));
        }
    }
    let mut stmt=db.prepare("SELECT source_id,source_version,byte_start,byte_end,text,index_text,task_id,id FROM source_segments
        WHERE owner_id=?1 AND generation=?2 AND (?3 IS NULL OR task_id=?3)")?;
    let mut rows = stmt.query(params![owner.0, i64::try_from(generation)?, task_id])?;
    let mut seen = 0;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let Some(source) = sources.get(id.as_str()) else {
            return Ok(false);
        };
        let Some(text) = body(source) else {
            return Ok(false);
        };
        let version: u64 = u64::try_from(row.get::<_, i64>(1)?)?;
        let start = usize::try_from(row.get::<_, i64>(2)?)?;
        let end = usize::try_from(row.get::<_, i64>(3)?)?;
        let part: String = row.get(4)?;
        let words: String = row.get(5)?;
        let stored_task: Option<String> = row.get(6)?;
        if source.index["source_version"] != version
            || task(source) != stored_task.as_deref()
            || !expected.contains(&(id.as_str(), start, end))
            || text.get(start..end) != Some(part.as_str())
            || index_tokens(text, start, end).join(" ") != words
        {
            return Ok(false);
        }
        if !words.is_empty() {
            let visible:bool=db.query_row(
                "SELECT EXISTS(SELECT 1 FROM source_fts JOIN source_segments s ON s.id=source_fts.rowid
                 WHERE source_fts MATCH ?1 AND s.owner_id=?2 AND s.id=?3)",
                params![format!("\"{words}\""),owner.0,row.get::<_,i64>(7)?],|r|r.get(0))?;
            if !visible {
                return Ok(false);
            }
        }
        seen += 1;
    }
    Ok(seen == expected.len())
}

impl SqliteMemoryStore {
    /// W4 在组装的同一读事务里取全部命中：先映射、过滤、去重，再由记录预算截断。
    /// 沿用原文检索的实体、短语命中数及接收顺序排序；不以片段 LIMIT 限制记录数。
    pub(crate) fn search_w4_facts(
        &self,
        db: &Connection,
        state: &PersistedMemoryState,
        query: &str,
        eligible: &[&Memory],
    ) -> PersistenceResult<Vec<MemoryId>> {
        if !self.source_search_enabled {
            return Err("source_search_disabled".into());
        }
        let (generation, version, revision) =
            metadata(db, &self.owner)?.ok_or("source_index_missing")?;
        if version != VERSION || revision != state.source_revision {
            return Err("source_index_version_mismatch".into());
        }
        if !projection_complete(db, &self.owner, state, generation, revision, None)? {
            return Err("source_index_projection_incomplete".into());
        }
        let entities = state.entities.list_by_owner(&self.owner);
        let phrases = natural_phrases(query, &entities);
        if phrases.len() > 256 {
            return Err("natural query exceeds 256 phrases".into());
        }
        if phrases.is_empty() {
            return Ok(Vec::new());
        }
        let expression = phrases
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(" OR ");
        let query_hints = entity_hints(query, &entities);
        let mut statement = db.prepare(
            "SELECT s.source_id,s.byte_start,s.byte_end,
               (SELECT count(*) FROM json_each(?4) e WHERE EXISTS (
                 SELECT 1 FROM json_each(e.value) n
                 WHERE instr(' '||s.index_text||' ',' '||n.value||' ')>0)) AS query_entity_matches,
               (SELECT count(*) FROM json_each(?5) p
                 WHERE instr(' '||s.index_text||' ',' '||p.value||' ')>0) AS query_phrase_matches
             FROM source_fts JOIN source_segments s ON s.id=source_fts.rowid
             WHERE source_fts MATCH ?1 AND s.owner_id=?2 AND s.generation=?3
             ORDER BY query_entity_matches DESC,query_phrase_matches DESC,
               s.received_sequence DESC,s.source_id,s.byte_start",
        )?;
        let rows = statement.query_map(
            params![
                expression,
                self.owner.0,
                i64::try_from(generation)?,
                serde_json::to_string(&query_hints.values().collect::<Vec<_>>())?,
                serde_json::to_string(&phrases)?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        let allowed: BTreeSet<MemoryId> = eligible.iter().map(|memory| memory.id).collect();
        let mut seen = BTreeSet::new();
        let mut hits = Vec::new();
        // ponytail: 全部命中逐片段映射（命中数 × owner 记录数）；实测过慢再按来源缓存映射。
        for row in rows {
            let (id, start, end) = row?;
            let source = state
                .sources
                .values()
                .find(|source| source.id == id)
                .ok_or("indexed source missing")?;
            // 来源任务不代替记录任务范围；eligible 已按主体、关系、状态和可信任务过滤。
            let (links, _) = memory_links(
                state,
                &self.owner,
                source,
                usize::try_from(start)?,
                usize::try_from(end)?,
                None,
                None,
            )?;
            for link in links {
                let id: MemoryId = serde_json::from_value(link["memory_id"].clone())?;
                if allowed.contains(&id) && seen.insert(id) {
                    hits.push(id);
                }
            }
        }
        Ok(hits)
    }

    pub fn set_source_search_enabled(&mut self, enabled: bool) {
        self.source_search_enabled = enabled;
    }

    /// Explicit maintenance, never called from read-only search or Inspector.
    pub fn rebuild_source_index(&mut self) -> PersistenceResult<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut state = Self::load(&tx, &self.owner)?;
        rebuild(&tx, &self.owner, &mut state)?;
        Self::save(&tx, &self.owner, &state)?;
        tx.commit()?;
        self.refresh_fact_index_after_commit();
        Ok(())
    }

    /// Caller supplies trusted task scope, never a model-inferred authorization scope.
    pub fn search_sources(
        &self,
        query: &str,
        task_id: Option<&str>,
        limit: usize,
    ) -> PersistenceResult<Value> {
        let started = Instant::now();
        let result = self.search_sources_inner(query, task_id, limit);
        match &result {
            Ok(value) => crate::diagnostics::record("lexical_raw", || value.clone()),
            Err(_) => crate::diagnostics::record("lexical_raw", || {
                json!({"channel":"lexical_raw","status":"failed",
                "owner":self.owner,"task_id":task_id,"reason":"source_search_failed","duration_ms":started.elapsed().as_millis()})
            }),
        }
        result
    }

    fn search_sources_inner(
        &self,
        query: &str,
        task_id: Option<&str>,
        limit: usize,
    ) -> PersistenceResult<Value> {
        let tx = self.connection.unchecked_transaction()?;
        let state = Self::load(&tx, &self.owner)?;
        let result = self.search_source_snapshot(&tx, &state, query, task_id, limit, None, None)?;
        tx.commit()?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)] // One trusted read snapshot and its already-authorized fact IDs.
    pub(crate) fn search_source_snapshot(
        &self,
        db: &Connection,
        state: &PersistedMemoryState,
        query: &str,
        task_id: Option<&str>,
        limit: usize,
        primary_query: Option<&str>,
        fact_memory_ids: Option<&BTreeSet<MemoryId>>,
    ) -> PersistenceResult<Value> {
        let started = Instant::now();
        if !(1..=50).contains(&limit) {
            return Err("source search limit must be 1..=50".into());
        }
        if task_id.is_some_and(|s| s.trim().is_empty() || s.chars().count() > 200) {
            return Err("invalid task scope".into());
        }
        let mut result = json!({"channel":"lexical_raw","owner":self.owner,"task_id":task_id,"status":"disabled",
            "processing_version":VERSION,"coverage_complete":false,"candidates":[],"candidate_count":0,
            "backend_generation_calls":0,"limit":limit});
        result["fact_index"] = if self.fact_index_enabled && fact_memory_ids.is_some() {
            json!({"status":"unavailable","reason":"raw_index_unavailable","renderer_version":crate::fact_index::VERSION})
        } else {
            json!({"status":"disabled"})
        };
        if !self.source_search_enabled {
            result["reason"] = json!("read_switch_disabled");
            return Ok(result);
        }
        // A read transaction binds FTS candidates and authority checks to one consistent source revision.
        let entities = state.entities.list_by_owner(&self.owner);
        let (expression, phrases) = if primary_query.is_some() {
            let phrases = natural_phrases(query, &entities);
            if phrases.len() > 256 {
                return Err("natural query exceeds 256 phrases".into());
            }
            let expression = phrases
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(" OR ");
            (expression, phrases)
        } else {
            query_expression(query, state, &self.owner)?
        };
        result["query"] = json!(query);
        result["query_phrases"] = json!(phrases);
        result["alias_policy"] = json!(if primary_query.is_some() {
            "natural-v2"
        } else {
            "registered_exact_query_expansion_v1"
        });
        result["read_revision"] = json!(state.revision);
        result["source_revision"] = json!(state.source_revision);
        result["status"] = json!("incomplete");
        let Some((generation, processor, revision)) = metadata(db, &self.owner)? else {
            result["reason"] = json!("index_not_built");
            return Ok(result);
        };
        result["generation"] = json!(generation);
        if processor != VERSION || revision != state.source_revision {
            result["reason"] = json!("index_version_mismatch");
            return Ok(result);
        }
        let mut complete =
            projection_complete(db, &self.owner, state, generation, revision, task_id)?;
        let fact_hints = if self.fact_index_enabled && complete {
            if let Some(ids) = fact_memory_ids {
                match crate::fact_index::read(db, &self.owner, state, ids, task_id) {
                    Ok((hints, report)) => {
                        result["fact_index"] = report;
                        hints
                    }
                    Err(error) => {
                        result["fact_index"] = json!({"status":"unavailable","reason":error.to_string(),
                            "renderer_version":crate::fact_index::VERSION});
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let mut candidates = Vec::new();
        let query_hints = entity_hints(primary_query.unwrap_or(""), &entities);
        let context_hints =
            entity_hints(if primary_query.is_some() { query } else { "" }, &entities);
        let primary_phrases = natural_phrases(primary_query.unwrap_or(""), &entities);
        if let Some(primary_query) = primary_query {
            result["primary_query"] = json!(primary_query);
            result["ranking_policy"] = json!("literal-hints-v1");
            result["query_entity_ids"] = json!(query_hints.keys().collect::<Vec<_>>());
            result["query_context_entity_ids"] = json!(context_hints.keys().collect::<Vec<_>>());
        }
        // Rank within the authorized scope BEFORE LIMIT so newer generic hits cannot evict name matches.
        // ponytail: literal counts scan scoped FTS hits; optimize only if measured query latency requires it.
        // No all-user BM25 statistics or global hit counts enter the output or ranking.
        let raw_sql = "SELECT s.source_id,s.source_version,s.byte_start,s.byte_end,s.text,s.index_text,s.task_id,
               (SELECT count(*) FROM json_each(?6) e WHERE EXISTS (
                 SELECT 1 FROM json_each(e.value) n
                 WHERE instr(' '||s.index_text||' ',' '||n.value||' ')>0)) AS query_entity_matches,
               (SELECT count(*) FROM json_each(?7) p
                 WHERE instr(' '||s.index_text||' ',' '||p.value||' ')>0) AS query_phrase_matches,
               (SELECT count(*) FROM json_each(?8) e WHERE EXISTS (
                 SELECT 1 FROM json_each(e.value) n
                 WHERE instr(' '||s.index_text||' ',' '||n.value||' ')>0)) AS query_context_entity_matches
             FROM source_fts JOIN source_segments s ON s.id=source_fts.rowid
             WHERE source_fts MATCH ?1 AND s.owner_id=?2 AND s.generation=?3
               AND (?4 IS NULL OR s.task_id=?4)
             ORDER BY query_entity_matches DESC,query_phrase_matches DESC,query_context_entity_matches DESC,
               s.received_sequence DESC,s.source_id,s.byte_start LIMIT ?5";
        // One effective key and one ranking/LIMIT. Only rule-eligible hints enter this CTE;
        // the sentinel prevents phrase matches crossing between complete fact items and raw tokens.
        let sql = if fact_hints.is_empty() {
            raw_sql.to_owned()
        } else {
            let select = raw_sql
                .replace("instr(' '||s.index_text", "instr(' '||s.key_text")
                .replace(
                    "FROM source_fts JOIN source_segments s ON s.id=source_fts.rowid",
                    "FROM keyed s",
                )
                .replace(
                    "WHERE source_fts MATCH ?1 AND s.owner_id=?2 AND s.generation=?3",
                    "WHERE s.owner_id=?2 AND s.generation=?3",
                );
            format!("WITH hints AS (
                SELECT json_extract(value,'$.source_id') source_id,
                  json_extract(value,'$.byte_start') byte_start,json_extract(value,'$.tokens') tokens
                FROM json_each(?9)),
              fact_keys AS (SELECT source_id,byte_start,group_concat(tokens,' zfactboundary ') tokens
                FROM hints GROUP BY source_id,byte_start),
              raw_hits AS (SELECT s.id FROM source_fts JOIN source_segments s ON s.id=source_fts.rowid
                WHERE source_fts MATCH ?1 AND s.owner_id=?2 AND s.generation=?3 AND (?4 IS NULL OR s.task_id=?4)),
              keyed AS (SELECT s.*,s.index_text||' zfactboundary '||coalesce(h.tokens,'') key_text
                FROM source_segments s LEFT JOIN fact_keys h ON h.source_id=s.source_id AND h.byte_start=s.byte_start
                WHERE s.owner_id=?2 AND s.generation=?3 AND (?4 IS NULL OR s.task_id=?4)
                AND (s.id IN (SELECT id FROM raw_hits) OR EXISTS (SELECT 1 FROM json_each(?10) p
                  WHERE instr(' '||h.tokens||' ',' '||p.value||' ')>0))) {select}")
        };
        let mut statement = db.prepare(&sql)?;
        // An empty token set is a completed empty search, never a match-all query.
        let expression = if expression.is_empty() {
            "\"__no_search_terms__\"".to_owned()
        } else {
            expression
        };
        result["match_expression"] = json!(expression);
        result["match_mode"] = json!("any_phrase");
        let mut parameters: Vec<rusqlite::types::Value> = vec![
            expression.into(),
            self.owner.0.clone().into(),
            i64::try_from(generation)?.into(),
            task_id
                .map(|s| s.to_owned().into())
                .unwrap_or(rusqlite::types::Value::Null),
            i64::try_from(limit)?.into(),
            serde_json::to_string(&query_hints.values().collect::<Vec<_>>())?.into(),
            serde_json::to_string(&primary_phrases)?.into(),
            serde_json::to_string(&context_hints.values().collect::<Vec<_>>())?.into(),
        ];
        if !fact_hints.is_empty() {
            parameters.push(serde_json::to_string(&fact_hints)?.into());
            parameters.push(serde_json::to_string(&phrases)?.into());
        }
        let rows = statement.query_map(rusqlite::params_from_iter(parameters), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
            ))
        })?;
        for row in rows {
            let (
                id,
                version,
                start,
                end,
                indexed_text,
                index_words,
                index_task,
                query_entity_matches,
                query_phrase_matches,
                query_context_entity_matches,
            ) = row?;
            let version = u64::try_from(version)?;
            let start = usize::try_from(start)?;
            let end = usize::try_from(end)?;
            let Some(source) = state.sources.values().find(|s| s.id == id) else {
                complete = false;
                continue;
            };
            let Some(text) = body(source) else {
                complete = false;
                continue;
            };
            if task(source) != index_task.as_deref()
                || task_id.is_some_and(|t| task(source) != Some(t))
                || source.index["status"] != "ready"
                || source.index["source_version"] != version
                || source.index["processing_version"] != VERSION
                || source.index["generation"] != generation
                || source.index["source_revision"] != revision
                || text.get(start..end) != Some(indexed_text.as_str())
                || index_tokens(text, start, end).join(" ") != index_words
                || indexed_text.chars().count() > SEGMENT_CHARS
            {
                complete = false;
                continue;
            }
            let (links, uncertain) =
                memory_links(state, &self.owner, source, start, end, task_id, Some(20))?;
            let positioned_words = format!(" {index_words} ");
            let matched_phrases: Vec<_> = phrases
                .iter()
                .filter(|p| positioned_words.contains(&format!(" {p} ")))
                .collect();
            candidates.push(json!({"source_id":id,"segment_id":format!("{id}:{version}:{VERSION}:{generation}:{start}"),
                "matched_query_phrases":matched_phrases,
                "segment_version":version,"byte_start":start,"byte_end":end,"text":indexed_text,
                "session_id":source.session_id,"turn_id":source.turn_id,"task_id":task(source),
                "observed_at":source.observed_at,"received_sequence":source.received_sequence,
                "role":"user","source_origin":source.request.as_ref().and_then(|r|r["source_origin"].as_str()).unwrap_or("mcp_received"),"evidence_kind":"raw_statement",
                "enrichment":source.enrichment,"known_memories":links,"mapping_uncertain":uncertain}));
            let hint_matches: Vec<_> = fact_hints.iter().filter(|h| h.source_id == id && h.byte_start == start)
                .filter_map(|h| {
                    let positioned = format!(" {} ",h.tokens);
                    let matched: Vec<_> = phrases.iter().filter(|p|positioned.contains(&format!(" {p} "))).collect();
                    (!matched.is_empty()).then(||json!({"memory_id":h.memory_id,"evidence_id":h.evidence_id,
                        "quote_byte_range":h.quote_byte_range,"renderer_version":crate::fact_index::VERSION,
                        "projection_revision":state.revision,"matched_query_phrases":matched,
                        "provenance":"committed_memory_projection"}))
                }).collect();
            if !hint_matches.is_empty() {
                candidates.last_mut().unwrap()["fact_hint_matches"] = json!(hint_matches);
            }
            if primary_query.is_some() {
                candidates.last_mut().unwrap()["ranking_signals"] = json!({
                    "query_entity_matches":query_entity_matches,
                    "query_phrase_matches":query_phrase_matches,
                    "query_context_entity_matches":query_context_entity_matches});
            }
        }
        drop(statement);
        result["status"] = json!(if complete { "completed" } else { "incomplete" });
        result["coverage_complete"] = json!(complete);
        result["reason"] = if complete {
            Value::Null
        } else {
            json!("stale_or_missing_projection")
        };
        result["candidate_count"] = json!(candidates.len());
        result["candidates"] = json!(candidates);
        result["sort_basis"] = if primary_query.is_some() {
            json!([
                "query_entity_matches_desc",
                "query_phrase_matches_desc",
                "query_context_entity_matches_desc",
                "received_sequence_desc",
                "source_id",
                "byte_start"
            ])
        } else {
            json!(["received_sequence_desc", "source_id", "byte_start"])
        };
        result["duration_ms"] = json!(started.elapsed().as_millis());
        Ok(result)
    }
}
