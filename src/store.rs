//! Repository 接口及用于领域测试的标准库内存实现。

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;

use crate::{
    Cardinality, Entity, EntityId, EntityResolution, Memory, MemoryId, Predicate,
    PredicateDefinition, UserId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreError(
    /// 面向调用方和测试输出的存储错误说明。
    pub String,
);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for StoreError {}

pub trait MemoryRepository {
    /// 分配在 repository 内唯一的版本 ID。
    fn allocate_id(&mut self) -> Result<MemoryId, StoreError>;
    fn insert(&mut self, memory: Memory) -> Result<(), StoreError>;
    fn update(&mut self, memory: Memory) -> Result<(), StoreError>;
    fn get(&self, id: MemoryId) -> Result<Option<Memory>, StoreError>;
    fn list_by_owner(&self, owner_id: &UserId) -> Result<Vec<Memory>, StoreError>;
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
/// 确定性领域测试使用的内存 Memory Store。
pub struct InMemoryMemoryRepository {
    /// 下一次分配 ID 时使用的单调计数器。
    next_id: u64,
    /// 按 ID 排序保存的所有版本记录。
    memories: BTreeMap<MemoryId, Memory>,
}

impl MemoryRepository for InMemoryMemoryRepository {
    fn allocate_id(&mut self) -> Result<MemoryId, StoreError> {
        self.next_id += 1;
        Ok(MemoryId(self.next_id))
    }

    fn insert(&mut self, memory: Memory) -> Result<(), StoreError> {
        if self.memories.contains_key(&memory.id) {
            return Err(StoreError(format!("memory {} already exists", memory.id.0)));
        }
        self.memories.insert(memory.id, memory);
        Ok(())
    }

    fn update(&mut self, memory: Memory) -> Result<(), StoreError> {
        if !self.memories.contains_key(&memory.id) {
            return Err(StoreError(format!("memory {} does not exist", memory.id.0)));
        }
        self.memories.insert(memory.id, memory);
        Ok(())
    }

    fn get(&self, id: MemoryId) -> Result<Option<Memory>, StoreError> {
        Ok(self.memories.get(&id).cloned())
    }

    fn list_by_owner(&self, owner_id: &UserId) -> Result<Vec<Memory>, StoreError> {
        Ok(self
            .memories
            .values()
            .filter(|memory| &memory.owner_id == owner_id)
            .cloned()
            .collect())
    }
}

pub trait EntityRepository {
    fn upsert(&mut self, entity: Entity);
    fn get(&self, id: &EntityId) -> Option<&Entity>;
    fn resolve(&self, owner_id: &UserId, name_or_alias: &str) -> EntityResolution;
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
/// 支持 canonical name 与 aliases 精确解析的内存 Entity Store。
pub struct InMemoryEntityRepository {
    /// 以稳定 Entity ID 索引的实体集合。
    entities: HashMap<EntityId, Entity>,
}

impl InMemoryEntityRepository {
    pub(crate) fn remove(&mut self, id: &EntityId) -> Option<Entity> {
        self.entities.remove(id)
    }

    pub(crate) fn alias_conflicts(
        &self,
        proposed: &Entity,
        merging: Option<&EntityId>,
    ) -> Vec<Entity> {
        let mut conflicts: Vec<_> = self
            .entities
            .values()
            .filter(|entity| {
                entity.owner_id == proposed.owner_id
                    && entity.id != proposed.id
                    && Some(&entity.id) != merging
                    && std::iter::once(&proposed.canonical_name)
                        .chain(&proposed.aliases)
                        .any(|name| matches_entity_name(entity, name))
            })
            .cloned()
            .collect();
        conflicts.sort_by(|a, b| a.id.cmp(&b.id));
        conflicts
    }

    pub fn list_by_owner(&self, owner: &UserId) -> Vec<Entity> {
        let mut entities: Vec<_> = self
            .entities
            .values()
            .filter(|entity| &entity.owner_id == owner)
            .cloned()
            .collect();
        entities.sort_by(|a, b| a.id.cmp(&b.id));
        entities
    }
}

impl EntityRepository for InMemoryEntityRepository {
    fn upsert(&mut self, entity: Entity) {
        self.entities.insert(entity.id.clone(), entity);
    }

    fn get(&self, id: &EntityId) -> Option<&Entity> {
        self.entities.get(id)
    }

    fn resolve(&self, owner_id: &UserId, name_or_alias: &str) -> EntityResolution {
        let mut matches: Vec<_> = self
            .entities
            .values()
            .filter(|entity| {
                &entity.owner_id == owner_id && matches_entity_name(entity, name_or_alias)
            })
            .map(|entity| entity.id.clone())
            .collect();
        matches.sort();

        // 多个 alias 命中时显式返回 Ambiguous，调用方不得任选一个 Entity。
        match matches.len() {
            0 => EntityResolution::NotFound,
            1 => EntityResolution::Resolved(matches.remove(0)),
            _ => EntityResolution::Ambiguous(matches),
        }
    }
}

pub(crate) fn entity_name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn matches_entity_name(entity: &Entity, name: &str) -> bool {
    let key = entity_name_key(name);
    std::iter::once(&entity.canonical_name)
        .chain(&entity.aliases)
        .any(|name| entity_name_key(name) == key)
}

pub trait PredicateRegistry {
    /// 未注册 predicate 由调用方保守地按 Multiple 处理，避免误 supersede。
    fn definition(&self, predicate: &Predicate) -> Option<&PredicateDefinition>;
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
/// 测试和原型使用的内存 Predicate Registry。
pub struct InMemoryPredicateRegistry {
    /// Predicate 到其 cardinality definition 的映射。
    definitions: HashMap<Predicate, PredicateDefinition>,
}

impl InMemoryPredicateRegistry {
    pub fn register(&mut self, predicate: Predicate, cardinality: Cardinality) {
        self.definitions.insert(
            predicate.clone(),
            PredicateDefinition {
                predicate,
                cardinality,
            },
        );
    }
}

impl PredicateRegistry for InMemoryPredicateRegistry {
    fn definition(&self, predicate: &Predicate) -> Option<&PredicateDefinition> {
        self.definitions.get(predicate)
    }
}
