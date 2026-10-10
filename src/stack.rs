//! The Paste Stack queue: copies are collected, then each Cmd+V pastes the
//! next one in the chosen order.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackOrder {
    /// First copied, first pasted.
    #[default]
    OldestFirst,
    /// Last copied, first pasted.
    NewestFirst,
}

impl StackOrder {
    pub fn label(self) -> &'static str {
        match self {
            StackOrder::OldestFirst => "oldest first",
            StackOrder::NewestFirst => "newest first",
        }
    }
}

#[derive(Debug)]
pub struct StackQueue {
    order: StackOrder,
    /// Items in paste order: the front is pasted next.
    items: VecDeque<i64>,
}

impl StackQueue {
    pub fn new(order: StackOrder) -> StackQueue {
        StackQueue { order, items: VecDeque::new() }
    }

    pub fn order(&self) -> StackOrder {
        self.order
    }

    /// Adds a copied item. Returns the item the pasteboard must now hold,
    /// when that is not the item just copied (the pasteboard already has it).
    pub fn push(&mut self, id: i64) -> Option<i64> {
        match self.order {
            StackOrder::OldestFirst => self.items.push_back(id),
            StackOrder::NewestFirst => self.items.push_front(id),
        }
        self.head().filter(|head| *head != id)
    }

    /// The item the next Cmd+V pastes.
    pub fn head(&self) -> Option<i64> {
        self.items.front().copied()
    }

    /// Called after a paste: drops the pasted item and returns the next one.
    pub fn advance(&mut self) -> Option<i64> {
        self.items.pop_front();
        self.head()
    }

    /// Items in the order they will be pasted.
    pub fn in_paste_order(&self) -> impl Iterator<Item = i64> + '_ {
        self.items.iter().copied()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oldest_first() {
        let mut q = StackQueue::new(StackOrder::OldestFirst);
        assert_eq!(q.push(1), None, "the pasteboard already holds the only item");
        assert_eq!(q.push(2), Some(1), "the pasteboard must go back to the oldest");
        assert_eq!(q.push(3), Some(1));
        assert_eq!(q.in_paste_order().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(q.advance(), Some(2));
        assert_eq!(q.advance(), Some(3));
        assert_eq!(q.advance(), None);
    }

    #[test]
    fn newest_first() {
        let mut q = StackQueue::new(StackOrder::NewestFirst);
        assert_eq!(q.push(1), None);
        assert_eq!(q.push(2), None, "the newest copy is already on the pasteboard");
        assert_eq!(q.push(3), None);
        assert_eq!(q.in_paste_order().collect::<Vec<_>>(), [3, 2, 1]);
        assert_eq!(q.advance(), Some(2));
        // A copy made mid-stack goes to the front.
        assert_eq!(q.push(4), None);
        assert_eq!(q.in_paste_order().collect::<Vec<_>>(), [4, 2, 1]);
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn serde_names() {
        #[derive(Serialize, Deserialize)]
        struct W {
            order: StackOrder,
        }
        let w: W = toml::from_str("order = \"newest_first\"").unwrap();
        assert_eq!(w.order, StackOrder::NewestFirst);
        assert_eq!(toml::to_string(&W { order: StackOrder::OldestFirst }).unwrap().trim(), "order = \"oldest_first\"");
    }
}
