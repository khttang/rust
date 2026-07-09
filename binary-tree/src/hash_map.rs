const INITIAL_CAPACITY: usize = 16;

pub struct HashMap<K, V> {
    buckets: Vec<Vec<(K, V)>>,
    count: usize,
}

impl<K: Eq + std::hash::Hash, V> HashMap<K, V> {
    pub fn new() -> Self {
        let mut buckets = Vec::with_capacity(INITIAL_CAPACITY);
        for _ in 0..INITIAL_CAPACITY {
            buckets.push(Vec::new());
        }
        HashMap { buckets, count: 0 }
    }

    pub fn bucket_index(&self, key: &K) -> usize {
        use std::hash::{Hasher};
        use std::collections::hash_map::DefaultHasher;
        
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        (hasher.finish() as usize) % self.buckets.len()
    }

    pub fn insert(&mut self, key: K, value: V) {
        if self.count >= self.buckets.len() * 3 / 4 {
            self.resize();
        }

        let index = self.bucket_index(&key);
        let bucket = &mut self.buckets[index];
        for (existing_key, existing_value) in bucket.iter_mut() {
            if *existing_key == key {
                *existing_value = value;
                return;
            }
        }
        bucket.push((key, value));
        self.count += 1;
    }

    pub fn resize(&mut self) {
        let new_capacity = self.buckets.len() * 2;
        let mut new_buckets = Vec::with_capacity(new_capacity);
        for _ in 0..new_capacity {
            new_buckets.push(Vec::new());
        }

        let old_buckets = std::mem::replace(&mut self.buckets, new_buckets);
        self.count = 0;
        for bucket in old_buckets {
            for (key, value) in bucket {
                self.insert(key, value);
            }
        }
    }

    pub fn get(&self, key: K) -> Option<&V> {
        let index = self.bucket_index(&key);
        let bucket = &self.buckets[index];
        for (existing_key, existing_value) in bucket {
            if *existing_key == key {
                return Some(existing_value);
            }
        }
        None
    }

    pub fn remove(&mut self, key: &K) {
        let index = self.hash(key);
        let bucket = &mut self.buckets[index];
        if let Some(pos) = bucket.iter().position(|(existing_key, _)| existing_key == key) {
            bucket.remove(pos);
            self.count -= 1;
        }
    }

    fn hash(&self, key: &K) -> usize {
        use std::hash::{Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        (hasher.finish() as usize) % self.buckets.len()
    }
}
