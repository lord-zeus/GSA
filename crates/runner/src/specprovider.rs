//! Engine spec resolution: where the `Engine` spec for a job comes from.
//!
//! * [`SpecRegistry`] — in-memory `engine_id -> Engine` map (tests).
//! * [`DiskSpecSource`] — reads the engine's `.ini` from a GSA install
//!   root listed at build time (production).
//!
//! Both are wrapped in [`SpecSource`], which the `WorkerPool` consults
//! at claim time to turn a job's `engine_id` into a parsed spec.

use std::collections::HashMap;
use std::path::Path;

use engine::enginespec::Engine;
use uuid::Uuid;

/// In-memory registry of pre-parsed engines, keyed by the `engines`
/// table's `id` column.
#[derive(Debug, Clone, Default)]
pub struct SpecRegistry {
    by_id: HashMap<Uuid, Engine>,
}

impl SpecRegistry {
    pub fn new() -> Self {
        Self { by_id: HashMap::new() }
    }
    pub fn insert(&mut self, id: Uuid, engine: Engine) {
        self.by_id.insert(id, engine);
    }
    pub fn get(&self, id: Uuid) -> Option<Engine> {
        self.by_id.get(&id).cloned()
    }
    /// Iterate all engines (helper for tests / diagnostics).
    pub fn all(&self) -> impl Iterator<Item = &Engine> {
        self.by_id.values()
    }
}

#[derive(Debug, Clone)]
pub enum SpecSource {
    /// In-memory registry for tests.
    Inline(SpecRegistry),
    /// On-disk `.ini` lookup from the GSA install.
    FromDisk(DiskSpecSource),
}

impl SpecSource {
    /// F#27 — the on-disk GSA roots (empty for the inline/fixture source).
    /// The runner uses these to load the `Email_Engines/*.ini` provider specs
    /// for the register → activate → login family.
    pub fn gsa_roots(&self) -> Vec<std::path::PathBuf> {
        match self {
            Self::FromDisk(src) => src.roots.clone(),
            Self::Inline(_) => Vec::new(),
        }
    }

    /// Resolve a job's `engine_id` to a parsed `Engine`. The `file_path`
    /// hint is used by the disk backend; the inline backend keys by id.
    pub fn resolve(&self, engine_id: Uuid, file_path: &str) -> Option<Engine> {
        match self {
            Self::Inline(reg) => reg.get(engine_id),
            Self::FromDisk(src) => {
                // `engines.file_path` is the relative path (e.g.
                // "Engines/Aardvark Topsites.ini"). Try each root.
                for root in &src.roots {
                    let candidate = root.join(file_path);
                    if candidate.exists() {
                        if let Ok(e) = engine::enginespec::parse_engine_file(Path::new(&candidate)) {
                            return Some(e);
                        }
                    }
                }
                engine::enginespec::parse_engine_file(Path::new(file_path)).ok()
            }
        }
    }
}

/// On-disk GSA install roots, tried in order.
#[derive(Debug, Clone)]
pub struct DiskSpecSource {
    pub roots: Vec<std::path::PathBuf>,
}

impl DiskSpecSource {
    pub fn new(roots: Vec<std::path::PathBuf>) -> Self {
        Self { roots }
    }
}
