use std::rc::{Rc};
use std::cell::{RefCell};
use std::fmt::{Debug};
use memory_stats::{memory_stats};
use std::collections::VecDeque;

use crate::doubly_linked_node::DLNode;

#[derive(Debug, Clone)]
pub struct Deque<T: Clone + Debug> {
    pub head: Option<Rc<RefCell<DLNode<T>>>>,
    pub tail: Option<Rc<RefCell<DLNode<T>>>>,
}

impl <T: Debug + Clone> Deque<T> {
    pub fn new() -> Self {
        Deque { head: None, tail: None }
    }

    pub fn append(&mut self, value: T) {
        let new_tail = Rc::new(RefCell::new(DLNode::new(value)));
        if self.tail.is_some() {
            self.tail.as_ref().unwrap().borrow_mut().next = Some(new_tail.clone());
            new_tail.borrow_mut().prev = Some(Rc::downgrade(&self.tail.as_ref().unwrap()));
            self.tail = Some(new_tail);
        } else if self.head.is_some() {
            let mut head = self.head.as_ref().unwrap().borrow_mut();
            head.next = Some(new_tail.clone());
            new_tail.borrow_mut().prev = Some(Rc::downgrade(&self.head.as_ref().unwrap()));
            self.tail = Some(new_tail);
        } else {
            self.head = Some(new_tail);
            self.tail = None;
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        if let Some(node) = self.tail.take() {
            if Rc::ptr_eq(self.head.as_ref().unwrap(), &node.borrow().prev.as_ref().unwrap().upgrade().unwrap()) {
                let value: T = node.as_ref().borrow().data.to_owned();
                self.tail = None;
                self.head.as_ref().unwrap().borrow_mut().next = None;
                // println!("pop() - Number of references for head: {}.", Rc::strong_count(&self.head.as_ref().unwrap()));
                Some(value)
            } else {
                let new_tail = node.as_ref().borrow_mut().prev.as_ref().unwrap().upgrade().unwrap();
                new_tail.borrow_mut().next = None;
                self.tail = Some(new_tail);
                let value: T = node.as_ref().borrow().data.to_owned();
                //println!("pop() - Number of references for head: {}.", Rc::strong_count(&self.head.as_ref().unwrap()));
                Some(value)
            }
        } else {
            if self.head.is_some() {
                let head = self.head.take();
                let value = head.as_ref().unwrap().borrow().data.to_owned();
                self.tail = None;
                // If using Box in doubly_linked_list, add dereference (* operator) in the next line
                Some(value)
            } else {
                println!("pop() - head is None.");
                None
            }
        }
    }

    pub fn dequeue(&mut self) -> Option<T> {
        if let Some(head) = self.head.take() {
            let new_head = head.as_ref().borrow_mut().next.clone();
            if new_head.is_some() {
                self.head = new_head;
            } else {
                self.head = None;
            }
            let value = head.as_ref().borrow().data.to_owned();
            // If using Box in doubly_linked_list, add indirection (* operator) in the next line
            Some(value)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

#[test]
fn test_create_empty_deque() {
    let mut deque = Deque::<i32>::new();
    let result = deque.pop();
    assert_eq!(result, None);
}

#[test]
fn test_create_unitary_deque() {
    let mut deque = Deque::<i32>::new();
    deque.append(2);
    let result = deque.pop();
    assert_eq!(result, Some(2));
}

#[test]
fn test_create_deque_multiple_items() {
    let mut deque = Deque::<i32>::new();
    deque.append(3);
    deque.append(6);
    deque.append(9);
    deque.append(12);
    deque.append(15);
    deque.append(42);
    let mut results = vec![];
    results.push(deque.pop().unwrap());
    results.push(deque.pop().unwrap());
    results.push(deque.pop().unwrap());
    results.push(deque.pop().unwrap());
    results.push(deque.pop().unwrap());
    results.push(deque.pop().unwrap());
    assert_eq!(results, vec![42, 15, 12, 9, 6, 3]);
}

#[test]
fn test_create_large_deque() {
    if let Some(usage) = memory_stats() {
        println!("test_create_large_deque() - Physical memory usage before execution: {:.2} MB", usage.physical_mem as f32 / 1_000_000f32);
    }
    let (values, deque) = fill_deque();
    if let Some(usage) = memory_stats() {
        println!("test_create_large_deque() - Physical memory usage after execution: {:.2} MB", usage.physical_mem as f32 / 1_000_000f32);
    }
    let results = reverse_results(deque, values.len());
    assert!(values.iter().eq(results.iter()));
}

#[test]
fn test_create_empty_queue() {
    let mut deque = Deque::<i32>::new();
    let result = deque.dequeue();
    assert_eq!(result, None);
}

#[test]
fn test_create_unitary_queue() {
    let mut deque = Deque::<i32>::new();
    deque.append(2);
    let result = deque.dequeue();
    assert_eq!(result, Some(2));
}

#[test]
fn test_create_queue_multiple_items() {
    let mut deque = Deque::<i32>::new();
    deque.append(3);
    deque.append(6);
    deque.append(9);
    deque.append(12);
    deque.append(15);
    deque.append(42);
    let mut results = vec![];
    results.push(deque.dequeue().unwrap());
    results.push(deque.dequeue().unwrap());
    results.push(deque.dequeue().unwrap());
    results.push(deque.dequeue().unwrap());
    results.push(deque.dequeue().unwrap());
    results.push(deque.dequeue().unwrap());
    assert_eq!(results, vec![3, 6, 9, 12, 15, 42]);
}

#[test]
fn test_create_large_queue() {
    use rand::RngExt;
    let range = 10_000_000;
    let mut values: Vec<i32> = Vec::new();
    let mut rng = rand::rng();
    let mut deque = Deque::<i32>::new();
    if let Some(usage) = memory_stats() {
        println!("test_create_large_queue() - Physical memory usage before execution: {:.2} MB", usage.physical_mem as f32 / 1_000_000f32);
    }
    for _ in 0..range {
        let n = rng.random_range(0..=range);
        values.push(n);
        deque.append(n);
    }
    if let Some(usage) = memory_stats() {
        println!("test_create_large_queue() - Physical memory usage after execution: {:.2} MB", usage.physical_mem as f32 / 1_000_000f32);
    }
    let mut results: Vec<i32> = Vec::new();
    for _ in 0..range {
        results.push(deque.dequeue().unwrap());
    }
    assert!(results.iter().eq(values.iter()));
}

fn fill_deque() -> (Vec<i32>, Deque<i32>) {
    use rand::RngExt;
    let range = 10_000_000;
    let mut values: Vec<i32> = Vec::new();
    let mut rng = rand::rng();
    let mut deque = Deque::<i32>::new();
    if let Some(usage) = memory_stats() {
        println!("fill_deque() - Physical memory usage before execution: {:.2} MB", usage.physical_mem as f32 / 1_000_000f32);
    }
    for _ in 0..range {
        let n = rng.random_range(0..=range);
        values.push(n);
        deque.append(n);
    }
    if let Some(usage) = memory_stats() {
        println!("fill_deque() - Physical memory usage after execution: {:.2} MB", usage.physical_mem as f32 / 1_000_000f32);
    }
    (values, deque)
}

fn reverse_results(mut deque: Deque<i32>, length: usize) -> VecDeque<i32> {
    use std::collections::VecDeque;
    let mut results: VecDeque::<i32> = VecDeque::new();
    for _ in 0..length {
        results.push_front(deque.pop().unwrap());
    }
    results
}
}