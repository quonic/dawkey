// SPDX-License-Identifier: AGPL-3.0-or-later
//! The accessibility tree: what the UI shows, as data. MCP's `ui_tree` tool
//! returns it, and tests assert on it.

use serde::Serialize;

use crate::geometry::Rect;
use crate::node::{Chrome, Node, Role, WidgetId};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccessNode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<WidgetId>,
    pub role: Role,
    /// Accessible name: the explicit label, or the visible text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Bounds in logical pixels.
    pub bounds: Rect,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub clickable: bool,
    #[serde(skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chrome: Option<Chrome>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<AccessNode>,
}

fn is_true(b: &bool) -> bool {
    *b
}

impl AccessNode {
    /// Builds the tree, dropping anonymous layout containers (no ID, no
    /// text, plain group role) and splicing their children into the parent.
    pub(crate) fn build(root: &Node) -> AccessNode {
        let mut nodes = Vec::new();
        collect(root, &mut nodes);
        match nodes.len() {
            1 => nodes.pop().expect("one node"),
            _ => AccessNode {
                id: None,
                role: Role::Group,
                name: None,
                value: None,
                bounds: root.rect,
                clickable: false,
                enabled: true,
                checked: None,
                chrome: None,
                children: nodes,
            },
        }
    }

    /// Depth-first search by ID.
    pub fn find(&self, id: &str) -> Option<&AccessNode> {
        if self.id.as_ref().is_some_and(|i| i.as_str() == id) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }
}

fn collect(node: &Node, out: &mut Vec<AccessNode>) {
    let mut children = Vec::new();
    for child in &node.children {
        collect(child, &mut children);
    }
    let significant = node.id.is_some()
        || node.text.is_some()
        || node.label.is_some()
        || node.role != Role::Group;
    if !significant {
        out.extend(children);
        return;
    }
    out.push(AccessNode {
        id: node.id.clone(),
        role: node.role,
        name: node.label.clone().or_else(|| node.text.clone()),
        value: node.value.clone(),
        bounds: node.rect,
        clickable: node.clickable || node.chrome.is_some(),
        enabled: node.enabled,
        checked: node.checked,
        chrome: node.chrome,
        children,
    });
}
