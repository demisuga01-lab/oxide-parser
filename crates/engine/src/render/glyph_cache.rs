use std::collections::HashMap;

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

pub struct GlyphCache {
    entries: HashMap<GlyphCacheKey, CachedGlyph>,
    max_entries: usize,
}

impl GlyphCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(2048)
    }

    pub fn get(&self, key: &GlyphCacheKey) -> Option<&CachedGlyph> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: GlyphCacheKey, glyph: CachedGlyph) {
        if self.max_entries == 0 {
            return;
        }
        if self.entries.len() >= self.max_entries {
            // TODO(perf): replace with an LRU eviction policy.
            if let Some(oldest) = self.entries.keys().next().cloned() {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, glyph);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
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
        let cache = GlyphCache::new(100);
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
}
