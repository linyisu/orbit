//! Lazy syntax index. Callers must open/read through Workspace before lookup;
//! cache keys include content, so unchanged timestamps never hide an edit.
mod languages;
pub(crate) mod symbols;
pub(crate) use languages::language;

use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use symbols::AstSymbol;

const MAX_CACHED_FILES: usize = 256;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Identifier {
    pub name: String,
    pub line: usize,
}

#[derive(Debug)]
pub(crate) struct Source {
    pub symbols: Vec<AstSymbol>,
    pub identifiers: Vec<Identifier>,
    pub parsed: bool,
    pub truncated: bool,
}

impl Source {
    fn bytes(&self) -> usize {
        self.symbols
            .iter()
            .map(|s| s.name.capacity() + s.kind.capacity() + s.signature.capacity())
            .sum::<usize>()
            + self
                .identifiers
                .iter()
                .map(|s| s.name.capacity())
                .sum::<usize>()
            + self.symbols.capacity() * std::mem::size_of::<AstSymbol>()
            + self.identifiers.capacity() * std::mem::size_of::<Identifier>()
    }
}

struct Entry {
    hash: [u8; 32],
    source: Arc<Source>,
    bytes: usize,
    used: u64,
}

#[derive(Default)]
struct Cache {
    entries: BTreeMap<PathBuf, Entry>,
    bytes: usize,
    clock: u64,
}

#[derive(Clone, Default)]
pub struct RepositoryIndex(Arc<Mutex<Cache>>);

impl RepositoryIndex {
    pub(crate) fn analyze(&self, path: &Path, content: &str) -> Arc<Source> {
        let hash: [u8; 32] = Sha256::digest(content.as_bytes()).into();
        {
            let mut cache = self.0.lock().expect("repository cache poisoned");
            cache.clock = cache.clock.wrapping_add(1);
            let clock = cache.clock;
            if let Some(entry) = cache
                .entries
                .get_mut(path)
                .filter(|entry| entry.hash == hash)
            {
                entry.used = clock;
                return entry.source.clone();
            }
        }
        // Parse outside the lock. Concurrent misses may duplicate a parse, but
        // never block another file's analysis or exceed the retained budget.
        let source = Arc::new(symbols::analyze(path, content));
        let bytes = source.bytes() + path.as_os_str().len() + std::mem::size_of::<Entry>();
        let mut cache = self.0.lock().expect("repository cache poisoned");
        if let Some(old) = cache.entries.remove(path) {
            cache.bytes -= old.bytes;
        }
        if bytes <= MAX_CACHE_BYTES {
            while cache.bytes + bytes > MAX_CACHE_BYTES || cache.entries.len() >= MAX_CACHED_FILES {
                let Some(oldest) = cache
                    .entries
                    .iter()
                    .min_by_key(|(_, v)| v.used)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                let old = cache.entries.remove(&oldest).expect("entry exists");
                cache.bytes -= old.bytes;
            }
            let used = cache.clock;
            cache.entries.insert(
                path.to_owned(),
                Entry {
                    hash,
                    source: source.clone(),
                    bytes,
                    used,
                },
            );
            cache.bytes += bytes;
        }
        source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_index_refreshes_by_content_and_bounds_retained_memory() {
        let index = RepositoryIndex::default();
        let path = Path::new("same.rs");
        let first = index.analyze(path, "fn original() {}");
        assert!(Arc::ptr_eq(
            &first,
            &index.clone().analyze(path, "fn original() {}")
        ));
        let edited = index.analyze(path, "fn updated() {}");
        assert_eq!(edited.symbols[0].name, "updated");
        assert!(!Arc::ptr_eq(&first, &edited));
        for n in 0..MAX_CACHED_FILES * 2 {
            index.analyze(Path::new(&format!("file{n}.rs")), "fn sample() {}");
        }
        let cache = index.0.lock().unwrap();
        assert!(cache.entries.len() <= MAX_CACHED_FILES && cache.bytes <= MAX_CACHE_BYTES);
    }
}
