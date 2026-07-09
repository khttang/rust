#[derive(Debug)]
struct Node<T> {
    value: T,
    left: Option<Box<Node<T>>>,
    right: Option<Box<Node<T>>>,
}

#[derive(Debug)]
pub struct BinarySearchTree<T> {
    root: Option<Box<Node<T>>>,
}

impl<T: std::cmp::Ord + PartialEq + Clone> BinarySearchTree<T> {
    pub fn new() -> Self {
        BinarySearchTree { root: None }
    }

    pub fn insert(&mut self, value: T) {
        insert_node(&mut self.root, value);
    }

    pub fn contains(&self, value: T) -> bool {
        contains(&self.root, value)
    }

    pub fn sorted_values(&self) -> Vec<T> {
        let mut result = Vec::new();
        in_order(&self.root, &mut result);
        result
    }

    pub fn delete(&mut self, value: T) {
        delete_node(&mut self.root, value);
    }

}

fn contains<T: PartialEq + std::cmp::PartialOrd>(node: &Option<Box<Node<T>>>, value: T) -> bool {
    match node {
        None => false,
        Some(existing) => {
            if value == existing.value {
                true
            } else if value < existing.value {
                contains(&existing.left, value)
            } else {
                contains(&existing.right, value)
            }
        }
    }
}

fn in_order<T: Clone>(node: &Option<Box<Node<T>>>, result: &mut Vec<T>) {
    if let Some(n) = node {
        in_order(&n.left, result);
        result.push(n.value.clone());
        in_order(&n.right, result);
    }
}

fn insert_node<T: Ord>(node: &mut Option<Box<Node<T>>>, value: T) {
    match node {
        None => {
            // Found the empty spot, place the new node here
            *node = Some(Box::new(Node {
                value,
                left: None,
                right: None,
            }));
        }
        Some(existing) => {
            if value < existing.value {
                insert_node(&mut existing.left, value);
            } else if value > existing.value {
                insert_node(&mut existing.right, value);
            }
            // if value == existing.value, ignore duplicates
        }
    }
}

fn delete_node<T: Ord>(node: &mut Option<Box<Node<T>>>, value: T) {
    if let Some(n) = node {
        if value < n.value {
            delete_node(&mut n.left, value)
        } else if value > n.value {
            delete_node(&mut n.right, value)
        } else {
            // Found the node to delete
            *node = match (n.left.take(), n.right.take()) {
                (None, None) => None,           // leaf node, just remove it
                (Some(left), None) => Some(left), // one child, replace with it
                (None, Some(right)) => Some(right),
                (Some(mut left), Some(right)) => {
                    // Two children: find in-order successor (smallest in right subtree)
                    // and replace this node's value with it
                    // For brevity, merge right subtree into left's rightmost position
                    attach_right(&mut left, right);
                    Some(left)
                }
            };
        }
    }
}

fn attach_right<T: Ord>(node: &mut Box<Node<T>>, subtree: Box<Node<T>>) {
    if node.right.is_none() {
        node.right = Some(subtree);
    } else {
        attach_right(node.right.as_mut().unwrap(), subtree);
    }
}