//! The macOS device registry, as plain data.
//!
//! IOKit answers with its own reference counted types. This module holds the
//! shape that those answers turn into, so that every decision above it runs
//! on a host that has no IOKit at all.
//!
//! Nothing here names a type from `core-foundation` or `io-kit-sys`. Those
//! crates belong to one host, and a name from them here would stop this file
//! compiling on the other two, which is where two thirds of its tests run.

use std::collections::BTreeMap;

/// One value out of the registry.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Text(String),
    Data(Vec<u8>),
    Dict(BTreeMap<String, Value>),
    List(Vec<Value>),
}

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }
}

/// One node of the registry, and the node above it.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// The class of the node, such as `IOMedia` or `AppleAPFSVolume`.
    pub class: String,
    pub properties: BTreeMap<String, Value>,
    /// The node above this one in the service plane.
    pub parent: Option<usize>,
}

impl Node {
    pub fn new(class: &str) -> Self {
        Node {
            class: class.to_string(),
            properties: BTreeMap::new(),
            parent: None,
        }
    }

    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.properties.insert(key.to_string(), value);
        self
    }

    pub fn under(mut self, parent: usize) -> Self {
        self.parent = Some(parent);
        self
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.properties.get(key)
    }
}

/// What one walk of the registry found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegistrySnapshot {
    pub nodes: Vec<Node>,
    /// The nodes that are `IOMedia`, whole drives and partitions alike.
    pub media: Vec<usize>,
}

impl RegistrySnapshot {
    /// Walk from a node up to the root of the service plane.
    pub fn ancestors(&self, index: usize) -> impl Iterator<Item = &Node> + '_ {
        let mut next = self.nodes.get(index).and_then(|n| n.parent);
        std::iter::from_fn(move || {
            let here = next?;
            let node = self.nodes.get(here)?;
            next = node.parent;
            Some(node)
        })
    }

    /// Find a dictionary on a node or on the first ancestor that carries it.
    ///
    /// A drive keeps its name and its bus on the controller above it, and how
    /// far above depends on the machine.
    pub fn find_dict(&self, index: usize, key: &str) -> Option<&BTreeMap<String, Value>> {
        let own = self.nodes.get(index)?.get(key).and_then(Value::as_dict);
        if own.is_some() {
            return own;
        }
        self.ancestors(index)
            .find_map(|n| n.get(key).and_then(Value::as_dict))
    }

    /// The value of one property on a node.
    pub fn get(&self, index: usize, key: &str) -> Option<&Value> {
        self.nodes.get(index)?.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> RegistrySnapshot {
        // disk0 under a controller that carries the names.
        let controller = Node::new("IOBlockStorageDriver").with(
            "Device Characteristics",
            Value::Dict(BTreeMap::from([
                (
                    "Product Name".to_string(),
                    Value::Text("APPLE SSD".to_string()),
                ),
                ("Vendor Name".to_string(), Value::Text("Apple".to_string())),
            ])),
        );
        let whole = Node::new("IOMedia")
            .with("BSD Name", Value::Text("disk0".to_string()))
            .with("Whole", Value::Bool(true))
            .under(0);
        let part = Node::new("IOMedia")
            .with("BSD Name", Value::Text("disk0s2".to_string()))
            .with("Whole", Value::Bool(false))
            .under(1);
        RegistrySnapshot {
            nodes: vec![controller, whole, part],
            media: vec![1, 2],
        }
    }

    #[test]
    fn a_node_walks_up_to_the_root() {
        let t = tree();
        let classes: Vec<&str> = t.ancestors(2).map(|n| n.class.as_str()).collect();
        assert_eq!(classes, ["IOMedia", "IOBlockStorageDriver"]);
    }

    #[test]
    fn the_root_has_nothing_above_it() {
        let t = tree();
        assert_eq!(t.ancestors(0).count(), 0);
    }

    #[test]
    fn a_dictionary_is_found_on_a_node_far_above() {
        // The partition carries no name. The controller two steps up does.
        let t = tree();
        let d = t.find_dict(2, "Device Characteristics").unwrap();
        assert_eq!(d.get("Product Name").unwrap().as_text(), Some("APPLE SSD"));
    }

    #[test]
    fn a_dictionary_that_nobody_carries_is_absent() {
        let t = tree();
        assert!(t.find_dict(2, "Protocol Characteristics").is_none());
    }

    #[test]
    fn a_value_reads_back_as_the_kind_it_is_and_not_as_another() {
        let v = Value::Int(4096);
        assert_eq!(v.as_int(), Some(4096));
        assert_eq!(v.as_bool(), None);
        assert_eq!(v.as_text(), None);
    }

    #[test]
    fn a_node_that_does_not_exist_answers_nothing_rather_than_panicking() {
        let t = tree();
        assert!(t.get(99, "BSD Name").is_none());
        assert_eq!(t.ancestors(99).count(), 0);
    }
}
