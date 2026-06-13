//! 场景树：点分层级 ID 管理的保留模式图层集合。
//!
//! Artemis 用点分 ID 表达图层层级：`"1"` 是根组，`"1.0"` 是它的子层，`"1.0.-1"`
//! 再下一级。父层的变换/不透明度向下继承，`[lydel id="1.0"]` 删除整棵子树，
//! `[lyprop id="1.0"]` 对整个组批量设属性。本模块只维护这棵树的结构与每个节点
//! 的属性/动画，不涉及任何绘制——绘制由 `build` 遍历这棵树产出。

use crate::compositor::anim::Tween;
use crate::compositor::props::LayerProps;
use std::cmp::Ordering;
use std::collections::HashMap;

/// 一个图层节点。
///
/// 节点既可能绑定了纹理资源（`file`），也可能只是一个用于分组/变换的空容器
/// （只设了属性、没有 `file`）。
#[derive(Debug, Clone, Default)]
pub struct Layer {
    /// 完整点分 ID，如 `"1.0.-1"`。
    pub id: String,
    /// 绑定的逻辑资源名；`None` 表示纯分组节点。
    pub file: Option<String>,
    pub props: LayerProps,
    /// 作用在本节点属性上的进行中缓动。
    pub tweens: Vec<Tween>,
    /// 直接子节点的完整 ID，按插入顺序保存以保证稳定的绘制次序。
    pub children: Vec<String>,
}

impl Layer {
    fn new(id: String) -> Self {
        Self {
            id,
            ..Default::default()
        }
    }
}

/// Artemis 图层 ID 排序：按点号分割，数字部分按数值比较，字符串部分按字典序。
/// 数字部分优先于字符串部分（数字在前）。
fn compare_layer_id(a: &str, b: &str) -> Ordering {
    let parts_a: Vec<&str> = a.split('.').collect();
    let parts_b: Vec<&str> = b.split('.').collect();

    for i in 0.. {
        match (parts_a.get(i), parts_b.get(i)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(&pa), Some(&pb)) => {
                let ord = compare_id_part(pa, pb);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
    Ordering::Equal
}

/// 比较单个 ID 部分：数字按数值，字符串按字典序，数字优先于字符串。
fn compare_id_part(a: &str, b: &str) -> Ordering {
    let a_num: Option<i64> = a.parse().ok();
    let b_num: Option<i64> = b.parse().ok();

    match (a_num, b_num) {
        (Some(na), Some(nb)) => na.cmp(&nb),
        (Some(_), None) => Ordering::Less,      // 数字优先
        (None, Some(_)) => Ordering::Greater,   // 字符串在后
        (None, None) => a.cmp(b),               // 都是字符串，按字典序
    }
}

/// 整棵场景树。
///
/// 节点存在扁平的 `HashMap` 里（键为完整 ID），父子关系通过 ID 推导，子节点顺序
/// 单独记录。根节点集合是没有父级的顶层 ID，按插入顺序排列。
#[derive(Debug, Clone, Default)]
pub struct Scene {
    nodes: HashMap<String, Layer>,
    /// 顶层节点 ID，按插入顺序——决定根层之间的绘制先后。
    roots: Vec<String>,
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: &str) -> Option<&Layer> {
        self.nodes.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Layer> {
        self.nodes.get_mut(id)
    }

    /// 获取指定图层的子图层 ID，按 Artemis 图层顺序排序。
    pub fn children(&self, id: &str) -> Vec<String> {
        self.get(id)
            .map(|layer| {
                let mut sorted = layer.children.clone();
                sorted.sort_by(|a, b| compare_layer_id(a, b));
                sorted
            })
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 顶层节点 ID，按 Artemis 图层顺序排序（数字优先，数字按值，字符串按字典序）。
    pub fn roots(&self) -> Vec<String> {
        let mut sorted = self.roots.clone();
        sorted.sort_by(|a, b| compare_layer_id(a, b));
        sorted
    }

    /// 所有节点的 ID（无序），供需要遍历全树的调用方使用。
    pub fn iter_ids(&self) -> Vec<String> {
        self.nodes.keys().cloned().collect()
    }

    /// 确保某 ID 的节点存在（含祖先链接），不改动已有属性。
    pub fn ensure(&mut self, id: &str) {
        self.ensure_path(id);
    }

    /// 创建或替换一个图层。会按需创建缺失的祖先节点（作为纯分组容器），并把它
    /// 登记到父节点的子列表（或根列表）中。若 ID 已存在，则保留其在树中的位置，
    /// 只更新 `file`，属性留给后续 `[lyprop]` 设置。
    pub fn create(&mut self, id: &str, file: Option<String>) {
        self.ensure_path(id);
        if let Some(layer) = self.nodes.get_mut(id) {
            layer.file = file;
        }
    }

    /// 确保 `id` 及其所有祖先都作为节点存在，并接好父子链接。
    fn ensure_path(&mut self, id: &str) {
        if self.nodes.contains_key(id) {
            return;
        }

        match parent_id(id) {
            Some(parent) => {
                self.ensure_path(parent);
                self.nodes.insert(id.to_string(), Layer::new(id.to_string()));
                let parent_node = self
                    .nodes
                    .get_mut(parent)
                    .expect("父节点应已由 ensure_path 创建");
                if !parent_node.children.iter().any(|c| c == id) {
                    parent_node.children.push(id.to_string());
                }
            }
            None => {
                self.nodes.insert(id.to_string(), Layer::new(id.to_string()));
                if !self.roots.iter().any(|r| r == id) {
                    self.roots.push(id.to_string());
                }
            }
        }
    }

    /// 设置（合并）某图层的属性，会按需创建该节点。增量语义：只改动传入的键。
    pub fn set_props(&mut self, id: &str, raw: &HashMap<String, String>) {
        self.ensure_path(id);
        if let Some(layer) = self.nodes.get_mut(id) {
            layer.props.merge_raw(raw);
        }
    }

    /// 删除一个图层及其整棵子树，并从父节点/根列表中摘除。
    /// 返回被删除的节点数。
    pub fn delete(&mut self, id: &str) -> usize {
        if !self.nodes.contains_key(id) {
            return 0;
        }

        // 先从父节点的子列表（或根列表）里摘除自身。
        match parent_id(id) {
            Some(parent) => {
                if let Some(parent_node) = self.nodes.get_mut(parent) {
                    parent_node.children.retain(|c| c != id);
                }
            }
            None => self.roots.retain(|r| r != id),
        }

        self.remove_subtree(id)
    }

    /// 递归移除子树，返回移除的节点数。
    fn remove_subtree(&mut self, id: &str) -> usize {
        let children = match self.nodes.remove(id) {
            Some(node) => node.children,
            None => return 0,
        };
        let mut removed = 1;
        for child in children {
            removed += self.remove_subtree(&child);
        }
        removed
    }

    /// 重命名图层。把节点连同整棵子树搬到新 ID 前缀下，更新父链接。
    /// 新旧 ID 任一非法（如新 ID 已存在）时返回 `false`，不做改动。
    pub fn rename(&mut self, from: &str, to: &str) -> bool {
        if from == to {
            return true;
        }
        if !self.nodes.contains_key(from) || self.nodes.contains_key(to) {
            return false;
        }

        // 从旧父节点摘除。
        match parent_id(from) {
            Some(parent) => {
                if let Some(p) = self.nodes.get_mut(parent) {
                    p.children.retain(|c| c != from);
                }
            }
            None => self.roots.retain(|r| r != from),
        }

        // 递归改键，收集旧→新映射后重建。
        self.rekey_subtree(from, to);

        // 接到新父节点（或根）。
        self.ensure_parent_link(to);
        true
    }

    /// 把以 `from` 为根的子树整体改键到 `to` 前缀下。
    fn rekey_subtree(&mut self, from: &str, to: &str) {
        let mut node = match self.nodes.remove(from) {
            Some(n) => n,
            None => return,
        };
        let children = std::mem::take(&mut node.children);
        node.id = to.to_string();
        let new_children: Vec<String> = children
            .iter()
            .map(|child| {
                // 子 ID 形如 `from.suffix`，替换前缀。
                let suffix = &child[from.len()..];
                format!("{to}{suffix}")
            })
            .collect();
        node.children = new_children;
        self.nodes.insert(to.to_string(), node);

        for child in children {
            let suffix = &child[from.len()..];
            let new_child = format!("{to}{suffix}");
            self.rekey_subtree(&child, &new_child);
        }
    }

    /// 仅为已存在的节点补上父链接（rename 收尾用，不创建祖先）。
    fn ensure_parent_link(&mut self, id: &str) {
        match parent_id(id) {
            Some(parent) => {
                if let Some(p) = self.nodes.get_mut(parent) {
                    if !p.children.iter().any(|c| c == id) {
                        p.children.push(id.to_string());
                    }
                } else {
                    // 父节点不存在时，退化为根，避免悬挂。
                    if !self.roots.iter().any(|r| r == id) {
                        self.roots.push(id.to_string());
                    }
                }
            }
            None => {
                if !self.roots.iter().any(|r| r == id) {
                    self.roots.push(id.to_string());
                }
            }
        }
    }
}

/// 求点分 ID 的父 ID：`"1.0.-1"` → `"1.0"`，`"1"` → `None`。
fn parent_id(id: &str) -> Option<&str> {
    id.rfind('.').map(|pos| &id[..pos])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn create_auto_builds_ancestors() {
        let mut scene = Scene::new();
        scene.create("1.0.-1", Some("black".into()));
        // 祖先 "1" 与 "1.0" 应作为分组节点自动出现。
        assert!(scene.get("1").is_some());
        assert!(scene.get("1.0").is_some());
        assert_eq!(scene.get("1.0.-1").unwrap().file.as_deref(), Some("black"));
        assert_eq!(scene.roots(), &["1".to_string()]);
        assert_eq!(scene.get("1").unwrap().children, vec!["1.0".to_string()]);
    }

    #[test]
    fn delete_removes_whole_subtree() {
        let mut scene = Scene::new();
        scene.create("1.0", Some("a".into()));
        scene.create("1.0.0", Some("b".into()));
        scene.create("1.0.1", Some("c".into()));
        scene.create("2", Some("d".into()));

        let removed = scene.delete("1");
        assert_eq!(removed, 4); // 1, 1.0, 1.0.0, 1.0.1
        assert!(scene.get("1").is_none());
        assert!(scene.get("1.0.0").is_none());
        assert!(scene.get("2").is_some()); // 兄弟子树不受影响
        assert_eq!(scene.roots(), &["2".to_string()]);
    }

    #[test]
    fn set_props_is_incremental_and_autovivifies() {
        let mut scene = Scene::new();
        scene.set_props("5", &raw(&[("left", "10"), ("alpha", "255")]));
        scene.set_props("5", &raw(&[("alpha", "0")]));
        let p = &scene.get("5").unwrap().props;
        assert_eq!(p.left, Some(10.0));
        assert_eq!(p.alpha, Some(0));
    }

    #[test]
    fn insertion_order_preserved_for_children() {
        let mut scene = Scene::new();
        scene.create("1.2", Some("a".into()));
        scene.create("1.0", Some("b".into()));
        scene.create("1.1", Some("c".into()));
        // 子节点按创建顺序排列，而非数值/字典序。
        assert_eq!(
            scene.get("1").unwrap().children,
            vec!["1.2".to_string(), "1.0".to_string(), "1.1".to_string()]
        );
    }

    #[test]
    fn rename_moves_subtree() {
        let mut scene = Scene::new();
        scene.create("1.0", Some("a".into()));
        scene.create("1.0.0", Some("b".into()));
        assert!(scene.rename("1.0", "1.9"));

        assert!(scene.get("1.0").is_none());
        assert!(scene.get("1.0.0").is_none());
        assert_eq!(scene.get("1.9").unwrap().file.as_deref(), Some("a"));
        assert_eq!(scene.get("1.9.0").unwrap().file.as_deref(), Some("b"));
        assert!(scene.get("1").unwrap().children.contains(&"1.9".to_string()));
    }

    #[test]
    fn rename_rejects_existing_target() {
        let mut scene = Scene::new();
        scene.create("1.0", Some("a".into()));
        scene.create("1.1", Some("b".into()));
        assert!(!scene.rename("1.0", "1.1"));
        // 原节点保持不变。
        assert_eq!(scene.get("1.0").unwrap().file.as_deref(), Some("a"));
    }
}
