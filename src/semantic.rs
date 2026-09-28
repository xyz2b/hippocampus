//! V1 最小语义关系层：受控类别别名与显式 target IS-A 边。

use std::collections::{HashMap, HashSet};

use crate::MemoryValue;

/// 查询一个 target 是否等于或属于某个父类别。
pub trait SemanticRelationRegistry {
    fn is_a(&self, child: &MemoryValue, ancestor: &MemoryValue) -> bool;
}

#[derive(Debug, Default)]
/// 标准库内存实现；通过父类边的传递遍历回答 IS-A。
pub struct InMemorySemanticRelationRegistry {
    /// 规范化 child 到直接 parents 的邻接表。
    parents: HashMap<MemoryValue, HashSet<MemoryValue>>,
}

impl InMemorySemanticRelationRegistry {
    /// 注册一条直接 IS-A 边，例如 Latte → Coffee。
    pub fn register_is_a(&mut self, child: MemoryValue, parent: MemoryValue) {
        let child = normalized(child);
        let parent = normalized(parent);
        if child != parent {
            self.parents.entry(child).or_default().insert(parent);
        }
    }
}

impl SemanticRelationRegistry for InMemorySemanticRelationRegistry {
    fn is_a(&self, child: &MemoryValue, ancestor: &MemoryValue) -> bool {
        let child = normalized(child.clone());
        let ancestor = normalized(ancestor.clone());
        if child == ancestor {
            return true;
        }

        let mut pending = vec![child];
        let mut visited = HashSet::new();
        while let Some(current) = pending.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            for parent in self.parents.get(&current).into_iter().flatten() {
                if parent == &ancestor {
                    return true;
                }
                pending.push(parent.clone());
            }
        }
        false
    }
}

pub(crate) fn equivalent_values(left: &MemoryValue, right: &MemoryValue) -> bool {
    normalized(left.clone()) == normalized(right.clone())
}

fn normalized(value: MemoryValue) -> MemoryValue {
    match value {
        MemoryValue::Text(value) => {
            let value = value.trim().to_lowercase();
            // 仅折叠 V1 类别词表中的明确同义名称，不做子串匹配，不改写 Store 的原文值。
            // “咖啡因”不等于“咖啡”，产品 Entity 更不能降格成类别文本。
            let canonical = match value.as_str() {
                "咖啡" | "咖啡类饮料" | "咖啡饮料" | "coffee" | "coffee drinks" => {
                    "coffee"
                }
                "拿铁" | "拿铁咖啡" | "拿铁类咖啡饮料" | "latte" => "latte",
                "笔记本" | "笔记本电脑" | "laptop" => "laptop",
                _ => &value,
            };
            MemoryValue::Text(canonical.to_owned())
        }
        value => value,
    }
}
