
use std::rc::{Rc, Weak};
use std::cell::{RefCell};
use std::fmt::{Debug};

#[derive(Debug)]
pub struct DLNode<T: Clone + Debug> {
    pub data: T,
    pub prev: Option<Weak<RefCell<DLNode<T>>>>,
    pub next: Option<Rc<RefCell<DLNode<T>>>>,
}

impl <T: Clone + Debug> DLNode<T> {
    pub fn new(data: T) -> Self 
    where T: Clone + Debug {
        DLNode { data, next: None, prev: None }
    }
}