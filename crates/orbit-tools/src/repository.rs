//! Lazy syntax index. Callers must open/read through Workspace before lookup;
//! cache keys include content, so unchanged timestamps never hide an edit.
mod languages;
pub(crate) mod lexical;
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
    fn unsupported_sources_do_not_cache_their_token_stream() {
        let index = RepositoryIndex::default();
        let source = index.analyze(
            Path::new("large.java"),
            &format!(
                "class Engine {{\n{}\n}}",
                "local local 123 \"text\";\n".repeat(4000)
            ),
        );
        assert!(!source.parsed);
        assert_eq!(source.symbols[0].name, "Engine");
        assert!(source.identifiers.is_empty());
        assert!(source.bytes() < 2048);
    }

    #[test]
    fn imported_names_remain_reference_candidates() {
        let index = RepositoryIndex::default();
        let source = index.analyze(
            Path::new("caller.ts"),
            "import { resolve as run } from './engine';\nrun();\n",
        );
        assert!(
            source
                .identifiers
                .iter()
                .any(|id| id.name == "resolve" && id.line == 1)
        );
        assert!(
            source
                .identifiers
                .iter()
                .any(|id| id.name == "run" && id.line == 2)
        );
    }

    #[test]
    fn c_and_cpp_use_syntax_for_declarations_and_references() {
        let index = RepositoryIndex::default();
        for (path, text, expected) in [
            (
                "engine.c",
                "typedef int Count;\nstruct Engine { int value; };\nint resolve(int value) { return value; }\nint caller(void) { /* ignored */ return resolve(1); }\nconst char *text = \"resolve\";",
                vec!["Count", "Engine", "resolve", "caller"],
            ),
            (
                "engine.cpp",
                "namespace app {\nclass Engine { public: int resolve(int value); };\nint Engine::resolve(int value) { return value; }\nint caller(Engine &engine) { return engine.resolve(1); }\n}",
                vec!["Engine", "resolve", "resolve", "caller"],
            ),
        ] {
            let source = index.analyze(Path::new(path), text);
            assert!(source.parsed);
            assert_eq!(
                source
                    .symbols
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            let calls = source
                .identifiers
                .iter()
                .filter(|id| id.name == "resolve")
                .collect::<Vec<_>>();
            assert_eq!(calls.len(), 1, "{path}: {calls:?}");
            assert_eq!(calls[0].line, 4);
        }
    }

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
