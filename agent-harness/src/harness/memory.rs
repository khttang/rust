//! Schema-less store for learned environmental heuristics.
//!
//! [`AdaptiveMemoryLayer`] is a cheaply cloneable handle: clones share one
//! map, so every execution domain sees what any other has learned. Locks are
//! held only for the duration of a single map operation, never across an
//! `.await`.

use std::{
    collections::HashMap,
    sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use crate::models::ManifestEntry;

/// Limits applied by [`AdaptiveMemoryLayer::compact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionPolicy {
    /// Values longer than this many characters are truncated.
    pub max_value_chars: usize,
    /// Entries are evicted, largest first, until the sum of key and value
    /// lengths in bytes fits this budget.
    pub max_total_bytes: usize,
}

impl CompactionPolicy {
    /// A tight budget for small local models with short context windows.
    pub const SMALL_MODEL: Self = Self {
        max_value_chars: 256,
        max_total_bytes: 2048,
    };
}

/// What a [`AdaptiveMemoryLayer::compact`] pass changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionReport {
    /// Entries whose value became empty after whitespace normalisation.
    pub dropped_empty: usize,
    /// Entries whose value was normalised or truncated.
    pub rewritten: usize,
    /// Entries evicted to meet the byte budget.
    pub evicted: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

impl CompactionReport {
    pub fn changed(&self) -> bool {
        self.dropped_empty + self.rewritten + self.evicted > 0
    }
}

/// Shared, unstructured key-value memory.
#[derive(Debug, Clone, Default)]
pub struct AdaptiveMemoryLayer {
    entries: Arc<RwLock<HashMap<String, String>>>,
}

impl AdaptiveMemoryLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a heuristic, returning the value it replaced.
    pub fn learn(&self, key: impl Into<String>, value: impl Into<String>) -> Option<String> {
        self.write().insert(key.into(), value.into())
    }

    /// Store every manifest entry, returning how many were written.
    pub fn learn_manifest<'a>(
        &self,
        entries: impl IntoIterator<Item = &'a ManifestEntry>,
    ) -> usize {
        let mut map = self.write();
        let mut count = 0;
        for entry in entries {
            map.insert(entry.key.clone(), entry.value_text().into_owned());
            count += 1;
        }
        count
    }

    pub fn recall(&self, key: &str) -> Option<String> {
        self.read().get(key).cloned()
    }

    pub fn forget(&self, key: &str) -> Option<String> {
        self.write().remove(key)
    }

    pub fn clear(&self) {
        self.write().clear();
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// Sum of key and value lengths in bytes.
    pub fn total_bytes(&self) -> usize {
        total_bytes(&self.read())
    }

    /// Render all entries as `key: value` lines, sorted by key, for
    /// inclusion in a prompt. Sorting keeps prompts deterministic.
    pub fn render(&self) -> String {
        let map = self.read();
        let mut pairs: Vec<_> = map.iter().collect();
        pairs.sort_unstable_by(|a, b| a.0.cmp(b.0));

        let mut out = String::with_capacity(total_bytes(&map) + pairs.len() * 3);
        for (key, value) in pairs {
            out.push_str(key);
            out.push_str(": ");
            out.push_str(value);
            out.push('\n');
        }
        out
    }

    /// Strip context-window bloat in place:
    ///
    /// 1. collapse whitespace runs and trim each value;
    /// 2. drop entries left empty;
    /// 3. truncate values to `policy.max_value_chars`;
    /// 4. evict the largest entries (ties broken by key) until the store
    ///    fits `policy.max_total_bytes`.
    ///
    /// The result depends only on the contents and the policy, never on
    /// `HashMap` iteration order.
    pub fn compact(&self, policy: CompactionPolicy) -> CompactionReport {
        let mut map = self.write();
        let mut report = CompactionReport {
            bytes_before: total_bytes(&map),
            ..CompactionReport::default()
        };

        map.retain(|_, value| {
            let mut changed = false;
            if needs_normalizing(value) {
                *value = value.split_whitespace().collect::<Vec<_>>().join(" ");
                changed = true;
            }
            if value.is_empty() {
                report.dropped_empty += 1;
                return false;
            }
            if let Some((cut, _)) = value.char_indices().nth(policy.max_value_chars) {
                value.truncate(cut);
                value.truncate(value.trim_end().len());
                changed = true;
            }
            report.rewritten += usize::from(changed);
            true
        });

        let mut bytes = total_bytes(&map);
        if bytes > policy.max_total_bytes {
            let mut by_size: Vec<(usize, String)> = map
                .iter()
                .map(|(k, v)| (k.len() + v.len(), k.clone()))
                .collect();
            // Largest first; among equals, the greatest key goes first.
            by_size.sort_unstable_by(|a, b| b.cmp(a));
            for (size, key) in by_size {
                if bytes <= policy.max_total_bytes {
                    break;
                }
                map.remove(&key);
                bytes -= size;
                report.evicted += 1;
            }
        }

        report.bytes_after = bytes;
        report
    }

    // The map holds plain strings and every mutation is a single map call, so
    // a writer that panicked cannot leave it inconsistent: recover from poison.
    fn read(&self) -> RwLockReadGuard<'_, HashMap<String, String>> {
        self.entries.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, HashMap<String, String>> {
        self.entries.write().unwrap_or_else(PoisonError::into_inner)
    }
}

fn total_bytes(map: &HashMap<String, String>) -> usize {
    map.iter().map(|(k, v)| k.len() + v.len()).sum()
}

/// True if `s` has leading/trailing whitespace, a whitespace run, or any
/// whitespace other than a single ASCII space.
fn needs_normalizing(s: &str) -> bool {
    let mut prev_ws = true;
    for c in s.chars() {
        let ws = c.is_whitespace();
        if ws && (prev_ws || c != ' ') {
            return true;
        }
        prev_ws = ws;
    }
    prev_ws && !s.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn layer_is_send_sync_static() {
        fn assert_bounds<T: Clone + Send + Sync + 'static>() {}
        assert_bounds::<AdaptiveMemoryLayer>();
    }

    #[test]
    fn learn_recall_forget() {
        let mem = AdaptiveMemoryLayer::new();
        assert!(mem.is_empty());
        assert_eq!(mem.learn("os", "linux"), None);
        assert_eq!(mem.learn("os", "darwin").as_deref(), Some("linux"));
        assert_eq!(mem.recall("os").as_deref(), Some("darwin"));
        assert_eq!(mem.total_bytes(), "os".len() + "darwin".len());
        assert_eq!(mem.forget("os").as_deref(), Some("darwin"));
        assert!(mem.recall("os").is_none());
    }

    #[test]
    fn clones_share_state() {
        let a = AdaptiveMemoryLayer::new();
        let b = a.clone();
        a.learn("k", "v");
        assert_eq!(b.recall("k").as_deref(), Some("v"));
        b.clear();
        assert!(a.is_empty());
    }

    #[tokio::test]
    async fn concurrent_writers_from_tasks() {
        let mem = AdaptiveMemoryLayer::new();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let mem = mem.clone();
                tokio::spawn(async move { mem.learn(format!("k{i}"), i.to_string()) })
            })
            .collect();
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(mem.len(), 8);
    }

    #[test]
    fn survives_poisoned_lock() {
        let mem = AdaptiveMemoryLayer::new();
        mem.learn("k", "v");
        let poisoner = mem.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.write();
            panic!("poison the lock");
        })
        .join();
        assert_eq!(mem.recall("k").as_deref(), Some("v"));
    }

    #[test]
    fn render_is_sorted() {
        let mem = AdaptiveMemoryLayer::new();
        mem.learn("b", "2");
        mem.learn("a", "1");
        assert_eq!(mem.render(), "a: 1\nb: 2\n");
    }

    #[test]
    fn learns_manifest_entries() {
        let mem = AdaptiveMemoryLayer::new();
        let entries = [
            ManifestEntry::new("shell", "zsh"),
            ManifestEntry::new("cores", json!(8)),
        ];
        assert_eq!(mem.learn_manifest(&entries), 2);
        assert_eq!(mem.recall("shell").as_deref(), Some("zsh"));
        assert_eq!(mem.recall("cores").as_deref(), Some("8"));
    }

    #[test]
    fn normalizing_detection() {
        for s in ["a b", "abc", ""] {
            assert!(!needs_normalizing(s), "{s:?}");
        }
        for s in [" a", "a ", "a  b", "a\tb", "a\nb", " "] {
            assert!(needs_normalizing(s), "{s:?}");
        }
    }

    #[test]
    fn compact_normalizes_drops_and_truncates() {
        let mem = AdaptiveMemoryLayer::new();
        mem.learn("ws", "  lots \n\t of   space ");
        mem.learn("blank", " \n ");
        mem.learn("long", "abcdé fghij");
        mem.learn("clean", "ok");

        let report = mem.compact(CompactionPolicy {
            max_value_chars: 6,
            max_total_bytes: usize::MAX,
        });

        assert_eq!(mem.recall("ws").as_deref(), Some("lots o"));
        assert_eq!(mem.recall("blank"), None);
        // Cut lands after the space; the trailing space is trimmed and the
        // multi-byte `é` stays whole.
        assert_eq!(mem.recall("long").as_deref(), Some("abcdé"));
        assert_eq!(mem.recall("clean").as_deref(), Some("ok"));
        assert_eq!(report.dropped_empty, 1);
        assert_eq!(report.rewritten, 2);
        assert_eq!(report.evicted, 0);
        assert_eq!(report.bytes_after, mem.total_bytes());
        assert!(report.bytes_after < report.bytes_before);
    }

    #[test]
    fn compact_evicts_largest_first_deterministically() {
        let mem = AdaptiveMemoryLayer::new();
        mem.learn("a", "xxxx"); // 5 bytes
        mem.learn("b", "xxxx"); // 5 bytes, ties with `a`
        mem.learn("c", "xxxxxxxx"); // 9 bytes
        mem.learn("d", "x"); // 2 bytes

        let report = mem.compact(CompactionPolicy {
            max_value_chars: usize::MAX,
            max_total_bytes: 7,
        });

        // Evict `c` (9), then `b` (5, greater key than `a`): 21 -> 12 -> 7.
        assert_eq!(report.evicted, 2);
        assert_eq!(report.bytes_before, 21);
        assert_eq!(report.bytes_after, 7);
        assert!(mem.recall("a").is_some() && mem.recall("d").is_some());
    }

    #[test]
    fn compact_is_noop_within_budget() {
        let mem = AdaptiveMemoryLayer::new();
        mem.learn("k", "v");
        let report = mem.compact(CompactionPolicy::SMALL_MODEL);
        assert!(!report.changed());
        assert_eq!(report.bytes_before, report.bytes_after);
    }
}
