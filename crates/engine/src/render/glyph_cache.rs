use std::collections::{BTreeMap, HashMap};

use crate::render::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlyphCacheKey {
    /// Quick hash of the first 256 bytes of the font file.
    pub font_hash: u64,
    /// Unicode code point or glyph ID.
    pub code: u16,
    /// True when `code` is a glyph ID rather than a Unicode code point.
    pub is_gid: bool,
}

#[derive(Debug, Clone)]
pub struct CachedGlyph {
    /// Glyph outline in font units. None for spaces and glyphs with no outline.
    pub path: Option<Path>,
    /// Advance width in 1/1000 text units.
    pub advance_width: f64,
}

/// Glyph outline cache with **least-recently-used** eviction.
///
/// One cache is created per [`RenderState`](crate::render) (i.e. per
/// `render_page` call), so it is **per-thread scratch state** — never shared
/// across the rayon render threads introduced in the parallel-render work.
/// That means it needs no locking, and recency updates on a cache hit are
/// cheap unsynchronised writes.
///
/// Recency is tracked with a monotonic sequence number per entry plus a
/// `BTreeMap<seq, key>` index, giving O(log n) hit-recency updates and O(log n)
/// eviction of the genuinely least-recently-*used* entry (not merely the
/// least-recently-*inserted*, which is the behaviour this replaces). A hit on
/// an old-but-hot glyph refreshes its recency so it survives eviction — the
/// defining difference from the previous insertion-order policy.
pub struct GlyphCache {
    /// key -> (glyph, recency sequence number).
    entries: HashMap<GlyphCacheKey, (CachedGlyph, u64)>,
    /// recency sequence number -> key, ordered so the first entry is the LRU.
    order: BTreeMap<u64, GlyphCacheKey>,
    /// Next recency stamp to hand out. Strictly increasing; u64 never wraps in
    /// any realistic render (2^64 glyph accesses).
    next_seq: u64,
    max_entries: usize,
}

impl GlyphCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_seq: 0,
            max_entries,
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(2048)
    }

    /// Look up a glyph, marking it most-recently-used on a hit.
    ///
    /// Takes `&mut self` because an LRU read updates recency. Returns `None`
    /// on a miss without touching recency.
    pub fn get(&mut self, key: &GlyphCacheKey) -> Option<&CachedGlyph> {
        // Read the old recency stamp first, then release that borrow before
        // mutating `order`, then re-borrow mutably to write the new stamp.
        let old_seq = self.entries.get(key)?.1;
        let new_seq = self.next_seq;
        self.next_seq += 1;
        self.order.remove(&old_seq);
        self.order.insert(new_seq, key.clone());
        let entry = self
            .entries
            .get_mut(key)
            .expect("entry existed a moment ago");
        entry.1 = new_seq;
        Some(&entry.0)
    }

    pub fn insert(&mut self, key: GlyphCacheKey, glyph: CachedGlyph) {
        if self.max_entries == 0 {
            return;
        }

        // Overwriting an existing key: replace the value and refresh recency,
        // never counts toward eviction.
        if self.entries.contains_key(&key) {
            let old_seq = self.entries.get(&key).expect("contains_key just held").1;
            let new_seq = self.next_seq;
            self.next_seq += 1;
            self.order.remove(&old_seq);
            self.order.insert(new_seq, key.clone());
            let entry = self.entries.get_mut(&key).expect("contains_key just held");
            entry.0 = glyph;
            entry.1 = new_seq;
            return;
        }

        // New key: evict the least-recently-used entry if at capacity.
        if self.entries.len() >= self.max_entries {
            if let Some((&lru_seq, _)) = self.order.iter().next() {
                let lru_key = self
                    .order
                    .remove(&lru_seq)
                    .expect("iterator yielded this key");
                self.entries.remove(&lru_key);
            }
        }

        let seq = self.next_seq;
        self.next_seq += 1;
        self.order.insert(seq, key.clone());
        self.entries.insert(key, (glyph, seq));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Compute a quick FNV-1a hash of the first 256 bytes of font data.
    pub fn hash_font_bytes(bytes: &[u8]) -> u64 {
        const FNV_PRIME: u64 = 0x00000100000001B3;
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;

        let mut hash = FNV_OFFSET;
        for &byte in bytes.iter().take(256) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_cache_is_empty() {
        let cache = GlyphCache::new(100);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn insert_and_retrieve() {
        let mut cache = GlyphCache::new(100);
        let key = GlyphCacheKey {
            font_hash: 12345,
            code: 65,
            is_gid: false,
        };
        let glyph = CachedGlyph {
            path: None,
            advance_width: 500.0,
        };

        cache.insert(key.clone(), glyph);

        assert_eq!(cache.len(), 1);
        let retrieved = cache.get(&key).expect("cached glyph should exist");
        assert_eq!(retrieved.advance_width, 500.0);
        assert!(retrieved.path.is_none());
    }

    #[test]
    fn insert_up_to_capacity() {
        let mut cache = GlyphCache::new(3);
        for i in 0..3u16 {
            let key = GlyphCacheKey {
                font_hash: 0,
                code: i,
                is_gid: false,
            };
            let glyph = CachedGlyph {
                path: None,
                advance_width: f64::from(i),
            };
            cache.insert(key, glyph);
        }
        assert_eq!(cache.len(), 3, "cache should be at capacity");

        let key4 = GlyphCacheKey {
            font_hash: 0,
            code: 99,
            is_gid: false,
        };
        cache.insert(
            key4.clone(),
            CachedGlyph {
                path: None,
                advance_width: 99.0,
            },
        );

        assert_eq!(cache.len(), 3, "cache should still be at capacity");
        assert!(cache.get(&key4).is_some());
    }

    #[test]
    fn hash_font_bytes_is_deterministic() {
        let bytes = b"FAKE FONT BYTES FOR TESTING";
        assert_eq!(
            GlyphCache::hash_font_bytes(bytes),
            GlyphCache::hash_font_bytes(bytes)
        );
    }

    #[test]
    fn hash_font_bytes_differs_for_different_inputs() {
        let hash_a = GlyphCache::hash_font_bytes(b"FontA data...");
        let hash_b = GlyphCache::hash_font_bytes(b"FontB data...");
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn hash_font_bytes_handles_empty_input() {
        assert_eq!(GlyphCache::hash_font_bytes(&[]), 0xcbf29ce484222325u64);
    }

    #[test]
    fn cache_clear_empties_all_entries() {
        let mut cache = GlyphCache::new(100);
        cache.insert(
            GlyphCacheKey {
                font_hash: 0,
                code: 0,
                is_gid: false,
            },
            CachedGlyph {
                path: None,
                advance_width: 0.0,
            },
        );
        assert!(!cache.is_empty());
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn cached_glyph_with_path_is_retrieved_intact() {
        let mut cache = GlyphCache::new(100);
        let mut path = Path::new();
        path.move_to(0.0, 0.0);
        path.line_to(500.0, 0.0);
        path.line_to(250.0, 700.0);
        path.close();
        let key = GlyphCacheKey {
            font_hash: 99,
            code: 65,
            is_gid: false,
        };
        cache.insert(
            key.clone(),
            CachedGlyph {
                path: Some(path.clone()),
                advance_width: 600.0,
            },
        );

        let retrieved = cache.get(&key).expect("cached glyph should exist");
        assert!(retrieved.path.is_some());
        assert_eq!(retrieved.advance_width, 600.0);
        assert_eq!(
            retrieved
                .path
                .as_ref()
                .map(|cached_path| cached_path.segments.len()),
            Some(path.segments.len())
        );
    }

    #[test]
    fn cache_miss_returns_none() {
        let mut cache = GlyphCache::new(100);
        let key = GlyphCacheKey {
            font_hash: 0,
            code: 65,
            is_gid: false,
        };
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn different_char_codes_are_different_keys() {
        let mut cache = GlyphCache::new(100);
        let key_a = GlyphCacheKey {
            font_hash: 1,
            code: 65,
            is_gid: false,
        };
        let key_b = GlyphCacheKey {
            font_hash: 1,
            code: 66,
            is_gid: false,
        };
        cache.insert(
            key_a.clone(),
            CachedGlyph {
                path: None,
                advance_width: 600.0,
            },
        );
        cache.insert(
            key_b.clone(),
            CachedGlyph {
                path: None,
                advance_width: 580.0,
            },
        );
        assert_eq!(cache.get(&key_a).map(|g| g.advance_width), Some(600.0));
        assert_eq!(cache.get(&key_b).map(|g| g.advance_width), Some(580.0));
    }

    #[test]
    fn different_font_hashes_are_different_keys() {
        let mut cache = GlyphCache::new(100);
        let key_font1 = GlyphCacheKey {
            font_hash: 111,
            code: 65,
            is_gid: false,
        };
        let key_font2 = GlyphCacheKey {
            font_hash: 222,
            code: 65,
            is_gid: false,
        };
        cache.insert(
            key_font1.clone(),
            CachedGlyph {
                path: None,
                advance_width: 600.0,
            },
        );
        cache.insert(
            key_font2.clone(),
            CachedGlyph {
                path: None,
                advance_width: 540.0,
            },
        );
        assert_eq!(cache.get(&key_font1).map(|g| g.advance_width), Some(600.0));
        assert_eq!(cache.get(&key_font2).map(|g| g.advance_width), Some(540.0));
    }

    #[test]
    fn gid_and_char_code_keys_do_not_collide() {
        let key_char = GlyphCacheKey {
            font_hash: 1,
            code: 65,
            is_gid: false,
        };
        let key_gid = GlyphCacheKey {
            font_hash: 1,
            code: 65,
            is_gid: true,
        };

        assert_ne!(key_char, key_gid);
    }

    #[test]
    fn default_capacity_accepts_many_entries() {
        let mut cache = GlyphCache::with_default_capacity();
        for i in 0u16..100 {
            cache.insert(
                GlyphCacheKey {
                    font_hash: 1,
                    code: i,
                    is_gid: false,
                },
                CachedGlyph {
                    path: None,
                    advance_width: f64::from(i),
                },
            );
        }
        assert_eq!(cache.len(), 100);
    }

    #[test]
    fn hash_of_long_font_file_uses_only_first_256_bytes() {
        let mut data_a = vec![0u8; 1000];
        let mut data_b = vec![0u8; 1000];
        for i in 0..256 {
            data_a[i] = i as u8;
            data_b[i] = i as u8;
        }
        for i in 256..1000 {
            data_a[i] = 0;
            data_b[i] = 255;
        }
        assert_eq!(
            GlyphCache::hash_font_bytes(&data_a),
            GlyphCache::hash_font_bytes(&data_b)
        );
    }

    fn key(code: u16) -> GlyphCacheKey {
        GlyphCacheKey {
            font_hash: 7,
            code,
            is_gid: false,
        }
    }

    fn glyph(advance: f64) -> CachedGlyph {
        CachedGlyph {
            path: None,
            advance_width: advance,
        }
    }

    #[test]
    fn evicts_least_recently_used_not_least_recently_inserted() {
        // Capacity 3. Insert 0,1,2. Then ACCESS 0 (making it most-recent),
        // then insert 3 forcing one eviction. Under FIFO/insertion-order the
        // victim would be key 0 (oldest insert); under LRU the victim must be
        // key 1 (oldest *use*), and key 0 must survive.
        let mut cache = GlyphCache::new(3);
        cache.insert(key(0), glyph(0.0));
        cache.insert(key(1), glyph(1.0));
        cache.insert(key(2), glyph(2.0));

        // Touch key 0 -> now most-recently-used.
        assert_eq!(cache.get(&key(0)).map(|g| g.advance_width), Some(0.0));

        // Insert a fourth key -> evict the LRU, which is now key 1.
        cache.insert(key(3), glyph(3.0));

        assert_eq!(cache.len(), 3);
        assert!(
            cache.get(&key(0)).is_some(),
            "recently-accessed key 0 must survive LRU eviction"
        );
        assert!(
            cache.get(&key(1)).is_none(),
            "key 1 was least-recently-used and must be evicted"
        );
        assert!(cache.get(&key(2)).is_some());
        assert!(cache.get(&key(3)).is_some());
    }

    #[test]
    fn repeated_access_keeps_hot_entry_alive_across_many_evictions() {
        // A hot entry that is accessed every round must never be evicted even
        // as the rest of the cache churns completely.
        let mut cache = GlyphCache::new(4);
        cache.insert(key(1000), glyph(42.0)); // the hot entry

        for code in 0u16..50 {
            // Keep the hot entry warm before each new insert.
            assert_eq!(cache.get(&key(1000)).map(|g| g.advance_width), Some(42.0));
            cache.insert(key(code), glyph(f64::from(code)));
        }

        assert!(
            cache.get(&key(1000)).is_some(),
            "continuously-accessed entry must survive arbitrary churn"
        );
    }

    #[test]
    fn overwriting_existing_key_refreshes_recency_without_growing() {
        // Re-inserting an existing key must not count as a new entry and must
        // refresh its recency (so it is not the next eviction victim).
        let mut cache = GlyphCache::new(3);
        cache.insert(key(0), glyph(0.0));
        cache.insert(key(1), glyph(1.0));
        cache.insert(key(2), glyph(2.0));

        // Overwrite key 0 -> still 3 entries, key 0 now most-recent.
        cache.insert(key(0), glyph(100.0));
        assert_eq!(cache.len(), 3, "overwrite must not grow the cache");
        assert_eq!(cache.get(&key(0)).map(|g| g.advance_width), Some(100.0));

        // Next insert evicts the LRU (key 1), not the just-overwritten key 0.
        cache.insert(key(3), glyph(3.0));
        assert!(cache.get(&key(0)).is_some(), "refreshed key must survive");
        assert!(cache.get(&key(1)).is_none(), "true LRU (key 1) evicted");
    }

    #[test]
    fn zero_capacity_caches_nothing() {
        let mut cache = GlyphCache::new(0);
        cache.insert(key(0), glyph(0.0));
        assert_eq!(cache.len(), 0);
        assert!(cache.get(&key(0)).is_none());
    }
}
