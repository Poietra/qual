//! Persistent project-level analysis cache (DESIGN §9).
//!
//! Cache v2 keeps an exact whole-project entry for the fastest warm path and
//! dependency-closed component entries for incremental reuse after one source
//! component changes. Diagnostics, method summaries, and filesystem manifests
//! are JSON; ASTs and analyzed code are never serialized or executed. Cache
//! state remains disposable and is never required for correctness.

mod components;

pub(crate) use components::{AnalysisComponent, build as build_analysis_components};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::model::ResolvedConfig;
use crate::diagnostic::Diagnostic;
use crate::knowledge::KnowledgeProfile;
use crate::semantic::summaries::SummaryTable;

const CACHE_DIRECTORY: &str = ".manim-lint-cache";
const CACHE_DATABASE: &str = "cache-v2.sqlite3";
const CACHE_SCHEMA_VERSION: u32 = 1;
const CACHE_MAGIC: &[u8] = b"manim-lint/project-analysis-cache";
const PROJECT_KEY_MAGIC: &[u8] = b"whole-project";
const COMPONENT_KEY_MAGIC: &[u8] = b"dependency-component";
const MAX_PROJECT_ENTRIES: usize = 16;
const MAX_COMPONENT_ENTRIES: usize = 256;
const CACHE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const CACHE_OPEN_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// Whether a check used, populated, or deliberately bypassed the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// Cache use was disabled by a flag, an incompatible operation, or an
    /// environment error.
    Disabled,
    /// No valid entry existed; the full analysis ran.
    Miss,
    /// Some dependency components were reused and the changed components
    /// were analyzed again.
    Partial,
    /// A dependency-validated entry supplied the diagnostics.
    Hit,
}

/// Stable SHA-256 key for one complete project analysis input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey([u8; 32]);

/// Reusable facts owned by one dependency-closed source component.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ComponentCacheEntry {
    pub diagnostics: Vec<Diagnostic>,
    pub summaries: SummaryTable,
}

/// One filesystem path whose state can affect an otherwise source-identical
/// result (currently literal asset resolution and SVG content inspection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DependencyStamp {
    path: String,
    kind: DependencyKind,
    digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum DependencyKind {
    Missing,
    File,
    Directory,
    Other,
}

/// Dependency paths captured around a cold analysis and validated on lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyManifest {
    dependencies: Vec<DependencyStamp>,
}

impl DependencyManifest {
    /// Captures deterministic stamps for the supplied absolute paths.
    pub fn capture(paths: &BTreeSet<PathBuf>) -> Result<Self, CacheError> {
        let mut dependencies = Vec::with_capacity(paths.len());
        for path in paths {
            dependencies.push(stamp(path)?);
        }
        Ok(Self { dependencies })
    }

    fn is_current(&self) -> Result<bool, CacheError> {
        for expected in &self.dependencies {
            let current = stamp(Path::new(&expected.path))?;
            if current != *expected {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Best-effort `SQLite` cache session. Failures become warnings and disable the
/// cache for the current run; they never abort analysis.
pub struct AnalysisCache {
    path: PathBuf,
    connection: Option<Connection>,
    status: CacheStatus,
    warnings: Vec<String>,
    pending_component_touches: Vec<CacheKey>,
}

impl AnalysisCache {
    /// A session that performs no filesystem or `SQLite` work.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            path: PathBuf::new(),
            connection: None,
            status: CacheStatus::Disabled,
            warnings: Vec::new(),
            pending_component_touches: Vec::new(),
        }
    }

    /// Opens (or creates) `.manim-lint-cache/cache-v2.sqlite3` under the
    /// project root. Corrupt derivative state is removed and rebuilt once.
    #[must_use]
    pub fn open(project_root: &Path) -> Self {
        let path = project_root.join(CACHE_DIRECTORY).join(CACHE_DATABASE);
        match open_connection(&path) {
            Ok(connection) => Self {
                path,
                connection: Some(connection),
                status: CacheStatus::Miss,
                warnings: Vec::new(),
                pending_component_touches: Vec::new(),
            },
            Err(error) if error.is_corruption() => {
                let mut warnings = vec![format!(
                    "analysis cache was corrupt and is being rebuilt: {error}"
                )];
                if let Err(remove_error) = remove_database_files(&path) {
                    warnings.push(format!(
                        "analysis cache is disabled because the corrupt database could not be removed: {remove_error}"
                    ));
                    return Self {
                        path,
                        connection: None,
                        status: CacheStatus::Disabled,
                        warnings,
                        pending_component_touches: Vec::new(),
                    };
                }
                match open_connection(&path) {
                    Ok(connection) => Self {
                        path,
                        connection: Some(connection),
                        status: CacheStatus::Miss,
                        warnings,
                        pending_component_touches: Vec::new(),
                    },
                    Err(reopen_error) => {
                        warnings.push(format!(
                            "analysis cache is disabled after rebuild failed: {reopen_error}"
                        ));
                        Self {
                            path,
                            connection: None,
                            status: CacheStatus::Disabled,
                            warnings,
                            pending_component_touches: Vec::new(),
                        }
                    }
                }
            }
            Err(error) => Self {
                path,
                connection: None,
                status: CacheStatus::Disabled,
                warnings: vec![format!("analysis cache is disabled: {error}")],
                pending_component_touches: Vec::new(),
            },
        }
    }

    /// Returns a valid cached diagnostic set, if present.
    pub fn lookup(&mut self, key: &CacheKey) -> Option<Vec<Diagnostic>> {
        let result = self.lookup_inner(key);
        match result {
            Ok(Some(diagnostics)) => {
                self.status = CacheStatus::Hit;
                Some(diagnostics)
            }
            Ok(None) => {
                if self.connection.is_some() {
                    self.status = CacheStatus::Miss;
                }
                None
            }
            Err(error) => {
                self.handle_error(&error);
                None
            }
        }
    }

    /// Stores one complete diagnostic set and its filesystem dependencies.
    pub fn store(
        &mut self,
        key: &CacheKey,
        diagnostics: &[Diagnostic],
        dependencies: &DependencyManifest,
    ) {
        let result = self.store_inner(key, diagnostics, dependencies);
        if let Err(error) = result {
            self.handle_error(&error);
        }
    }

    /// Returns one dependency-validated incremental component entry.
    pub(crate) fn lookup_component(&mut self, key: &CacheKey) -> Option<ComponentCacheEntry> {
        match self.lookup_component_inner(key) {
            Ok(Some(entry)) => {
                self.pending_component_touches.push(key.clone());
                Some(entry)
            }
            Ok(None) => None,
            Err(error) => {
                self.handle_error(&error);
                None
            }
        }
    }

    /// Drops one structurally invalid component entry while keeping the rest
    /// of the disposable cache usable. The caller recomputes and stores this
    /// component again during the same check.
    pub(crate) fn reject_component(&mut self, key: &CacheKey, reason: &str) {
        let Some(connection) = &self.connection else {
            return;
        };
        match connection.execute(
            "DELETE FROM component_entries WHERE cache_key = ?1",
            params![key.0.as_slice()],
        ) {
            Ok(_) => self.warnings.push(format!(
                "analysis cache entry was corrupt and is being rebuilt: {reason}"
            )),
            Err(error) => self.handle_error(&CacheError::Sqlite(error)),
        }
    }

    /// Stores all cache-miss components in one `SQLite` transaction. A cold
    /// project with many independent modules pays one commit, not one per
    /// module; failure rolls back the complete batch.
    pub(crate) fn store_components(
        &mut self,
        entries: &[(CacheKey, ComponentCacheEntry, DependencyManifest)],
    ) {
        if let Err(error) = self.store_components_inner(entries) {
            self.handle_error(&error);
        }
    }

    /// Records whether component fallback reused none, some, or all shards.
    pub(crate) fn record_component_outcome(&mut self, hits: usize, total: usize) {
        if let Err(error) = self.touch_components() {
            self.handle_error(&error);
        }
        if self.connection.is_none() {
            return;
        }
        self.status = if total > 0 && hits == total {
            CacheStatus::Hit
        } else if hits > 0 {
            CacheStatus::Partial
        } else {
            CacheStatus::Miss
        };
    }

    #[must_use]
    pub const fn status(&self) -> CacheStatus {
        self.status
    }

    /// Drains warnings produced while opening, validating, or writing.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// Disables this session after a non-SQLite cache-preparation failure.
    pub fn disable_with_warning(&mut self, warning: String) {
        self.connection.take();
        self.status = CacheStatus::Disabled;
        self.warnings.push(warning);
    }

    fn lookup_inner(&mut self, key: &CacheKey) -> Result<Option<Vec<Diagnostic>>, CacheError> {
        let Some(connection) = &mut self.connection else {
            return Ok(None);
        };
        let cached: Option<(String, String)> = connection
            .query_row(
                "SELECT diagnostics_json, dependencies_json
                 FROM analysis_entries WHERE cache_key = ?1",
                params![key.0.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((diagnostics_json, dependencies_json)) = cached else {
            return Ok(None);
        };
        let dependencies: DependencyManifest = serde_json::from_str(&dependencies_json)?;
        if !dependencies.is_current()? {
            connection.execute(
                "DELETE FROM analysis_entries WHERE cache_key = ?1",
                params![key.0.as_slice()],
            )?;
            return Ok(None);
        }
        let diagnostics = serde_json::from_str(&diagnostics_json)?;
        let transaction = connection.transaction()?;
        let last_access = next_access(&transaction)?;
        transaction.execute(
            "UPDATE analysis_entries SET last_access = ?2 WHERE cache_key = ?1",
            params![key.0.as_slice(), last_access],
        )?;
        transaction.commit()?;
        Ok(Some(diagnostics))
    }

    fn store_inner(
        &mut self,
        key: &CacheKey,
        diagnostics: &[Diagnostic],
        dependencies: &DependencyManifest,
    ) -> Result<(), CacheError> {
        let diagnostics_json = serde_json::to_string(diagnostics)?;
        let dependencies_json = serde_json::to_string(dependencies)?;
        let Some(connection) = &mut self.connection else {
            return Ok(());
        };
        let transaction = connection.transaction()?;
        let last_access = next_access(&transaction)?;
        transaction.execute(
            "INSERT INTO analysis_entries(
                 cache_key, diagnostics_json, dependencies_json, last_access
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(cache_key) DO UPDATE SET
                 diagnostics_json = excluded.diagnostics_json,
                 dependencies_json = excluded.dependencies_json,
                 last_access = excluded.last_access",
            params![
                key.0.as_slice(),
                diagnostics_json,
                dependencies_json,
                last_access
            ],
        )?;
        transaction.execute(
            "DELETE FROM analysis_entries
             WHERE cache_key IN (
                 SELECT cache_key FROM analysis_entries
                 ORDER BY last_access DESC, cache_key DESC
                 LIMIT -1 OFFSET ?1
            )",
            params![i64::try_from(MAX_PROJECT_ENTRIES).expect("cache entry bound fits i64")],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn lookup_component_inner(
        &self,
        key: &CacheKey,
    ) -> Result<Option<ComponentCacheEntry>, CacheError> {
        let Some(connection) = &self.connection else {
            return Ok(None);
        };
        let cached: Option<(String, String, String)> = connection
            .query_row(
                "SELECT diagnostics_json, summaries_json, dependencies_json
                 FROM component_entries WHERE cache_key = ?1",
                params![key.0.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((diagnostics_json, summaries_json, dependencies_json)) = cached else {
            return Ok(None);
        };
        let dependencies: DependencyManifest = serde_json::from_str(&dependencies_json)?;
        if !dependencies.is_current()? {
            connection.execute(
                "DELETE FROM component_entries WHERE cache_key = ?1",
                params![key.0.as_slice()],
            )?;
            return Ok(None);
        }
        let entry = ComponentCacheEntry {
            diagnostics: serde_json::from_str(&diagnostics_json)?,
            summaries: serde_json::from_str(&summaries_json)?,
        };
        Ok(Some(entry))
    }

    fn touch_components(&mut self) -> Result<(), CacheError> {
        if self.pending_component_touches.is_empty() {
            return Ok(());
        }
        let Some(connection) = &mut self.connection else {
            self.pending_component_touches.clear();
            return Ok(());
        };
        let transaction = connection.transaction()?;
        // Components read by one project check form one LRU generation; their
        // order within that generation has no semantic value.
        let last_access = next_access(&transaction)?;
        for key in self.pending_component_touches.drain(..) {
            transaction.execute(
                "UPDATE component_entries SET last_access = ?2 WHERE cache_key = ?1",
                params![key.0.as_slice(), last_access],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn store_components_inner(
        &mut self,
        entries: &[(CacheKey, ComponentCacheEntry, DependencyManifest)],
    ) -> Result<(), CacheError> {
        if entries.is_empty() || self.connection.is_none() {
            return Ok(());
        }
        let serialized: Vec<(&CacheKey, String, String, String)> = entries
            .iter()
            .map(|(key, entry, dependencies)| {
                Ok((
                    key,
                    serde_json::to_string(&entry.diagnostics)?,
                    serde_json::to_string(&entry.summaries)?,
                    serde_json::to_string(dependencies)?,
                ))
            })
            .collect::<Result<_, CacheError>>()?;
        let Some(connection) = &mut self.connection else {
            return Ok(());
        };
        let transaction = connection.transaction()?;
        let last_access = next_access(&transaction)?;
        for (key, diagnostics_json, summaries_json, dependencies_json) in serialized {
            transaction.execute(
                "INSERT INTO component_entries(
                     cache_key, diagnostics_json, summaries_json,
                     dependencies_json, last_access
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(cache_key) DO UPDATE SET
                     diagnostics_json = excluded.diagnostics_json,
                     summaries_json = excluded.summaries_json,
                     dependencies_json = excluded.dependencies_json,
                     last_access = excluded.last_access",
                params![
                    key.0.as_slice(),
                    diagnostics_json,
                    summaries_json,
                    dependencies_json,
                    last_access
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM component_entries
             WHERE cache_key IN (
                 SELECT cache_key FROM component_entries
                 ORDER BY last_access DESC, cache_key DESC
                 LIMIT -1 OFFSET ?1
             )",
            params![i64::try_from(MAX_COMPONENT_ENTRIES).expect("component cache bound fits i64")],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn handle_error(&mut self, error: &CacheError) {
        self.connection.take();
        self.pending_component_touches.clear();
        if error.is_corruption() {
            self.warnings.push(format!(
                "analysis cache was corrupt and is being rebuilt: {error}"
            ));
            if let Err(remove_error) = remove_database_files(&self.path) {
                self.warnings.push(format!(
                    "analysis cache is disabled because rebuild cleanup failed: {remove_error}"
                ));
                self.status = CacheStatus::Disabled;
                return;
            }
            match open_connection(&self.path) {
                Ok(connection) => {
                    self.connection = Some(connection);
                    self.status = CacheStatus::Miss;
                }
                Err(reopen_error) => {
                    self.warnings.push(format!(
                        "analysis cache is disabled after rebuild failed: {reopen_error}"
                    ));
                    self.status = CacheStatus::Disabled;
                }
            }
        } else {
            self.warnings
                .push(format!("analysis cache is disabled: {error}"));
            self.status = CacheStatus::Disabled;
        }
    }
}

/// Builds the stable key for a complete source snapshot.
pub fn build_key(
    project_root: &Path,
    config: &ResolvedConfig,
    profile: &KnowledgeProfile,
    sources: &[(&Path, &[u8])],
) -> Result<CacheKey, CacheError> {
    let mut digest = Sha256::new();
    update_key_prefix(&mut digest, PROJECT_KEY_MAGIC, config, profile)?;
    update_sources(&mut digest, project_root, sources);
    Ok(CacheKey(digest.finalize().into()))
}

/// Builds the stable key for one dependency-closed component. The complete
/// sorted project layout protects serialized `FileId`s and module ownership;
/// only the component members contribute source bytes, allowing unrelated
/// edits to reuse this entry.
pub(crate) fn build_component_key(
    project_root: &Path,
    config: &ResolvedConfig,
    profile: &KnowledgeProfile,
    project_layout: &[&Path],
    sources: &[(&Path, &[u8])],
) -> Result<CacheKey, CacheError> {
    let mut digest = Sha256::new();
    update_key_prefix(&mut digest, COMPONENT_KEY_MAGIC, config, profile)?;
    update_field(
        &mut digest,
        &u64::try_from(project_layout.len())
            .expect("source layout count fits u64")
            .to_le_bytes(),
    );
    for path in project_layout {
        let relative = crate::source::relative_posix_path(project_root, path);
        update_field(&mut digest, relative.as_bytes());
    }
    update_sources(&mut digest, project_root, sources);
    Ok(CacheKey(digest.finalize().into()))
}

fn update_key_prefix(
    digest: &mut Sha256,
    kind: &[u8],
    config: &ResolvedConfig,
    profile: &KnowledgeProfile,
) -> Result<(), CacheError> {
    update_field(digest, CACHE_MAGIC);
    update_field(digest, kind);
    update_field(digest, &CACHE_SCHEMA_VERSION.to_le_bytes());
    update_field(digest, crate::VERSION.as_bytes());
    update_field(
        digest,
        option_env!("MANIM_LINT_BUILD_ID")
            .unwrap_or(crate::VERSION)
            .as_bytes(),
    );
    update_field(digest, &serde_json::to_vec(config)?);
    update_field(digest, &serde_json::to_vec(profile)?);
    Ok(())
}

fn update_sources(digest: &mut Sha256, project_root: &Path, sources: &[(&Path, &[u8])]) {
    update_field(
        digest,
        &u64::try_from(sources.len())
            .expect("source count fits u64")
            .to_le_bytes(),
    );
    for (path, bytes) in sources {
        let relative = crate::source::relative_posix_path(project_root, path);
        update_field(digest, relative.as_bytes());
        update_field(digest, bytes);
    }
}

fn update_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(
        u64::try_from(bytes.len())
            .expect("cache-key field length fits u64")
            .to_le_bytes(),
    );
    digest.update(bytes);
}

fn open_connection(path: &Path) -> Result<Connection, CacheError> {
    let directory = path.parent().expect("cache database has a parent");
    std::fs::create_dir_all(directory).map_err(|source| CacheError::Io {
        path: directory.to_path_buf(),
        source,
    })?;

    // SQLite's busy handler does not cover every SQLITE_LOCKED result. In
    // particular, processes that create the same database concurrently can
    // race while changing the initial journal mode to WAL. Retry the complete
    // connection setup so a transient first-open race does not disable the
    // cache for one of the callers.
    let deadline = Instant::now() + CACHE_BUSY_TIMEOUT;
    loop {
        match open_connection_once(path) {
            Ok(connection) => return Ok(connection),
            Err(error) if error.is_contention() && Instant::now() < deadline => {
                std::thread::sleep(CACHE_OPEN_RETRY_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
}

fn open_connection_once(path: &Path) -> Result<Connection, CacheError> {
    let mut connection = Connection::open(path)?;
    connection.busy_timeout(CACHE_BUSY_TIMEOUT)?;
    let journal_mode: String =
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        connection.pragma_update(None, "journal_mode", "WAL")?;
    }
    connection.pragma_update(None, "synchronous", "NORMAL")?;

    // Check and migrate the schema only after taking the write reservation.
    // Without this transaction, concurrent creators can all observe version
    // zero and one may drop tables that another has just initialized.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: u32 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != CACHE_SCHEMA_VERSION {
        transaction.execute_batch(
            "DROP TABLE IF EXISTS analysis_entries;
             DROP TABLE IF EXISTS component_entries;
             DROP TABLE IF EXISTS cache_metadata;",
        )?;
    }
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS analysis_entries (
             cache_key BLOB PRIMARY KEY NOT NULL,
             diagnostics_json TEXT NOT NULL,
             dependencies_json TEXT NOT NULL,
             last_access INTEGER NOT NULL
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS component_entries (
             cache_key BLOB PRIMARY KEY NOT NULL,
             diagnostics_json TEXT NOT NULL,
             summaries_json TEXT NOT NULL,
             dependencies_json TEXT NOT NULL,
             last_access INTEGER NOT NULL
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS cache_metadata (
             singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
             access_counter INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO cache_metadata(singleton, access_counter)
         VALUES (1, 0);",
    )?;
    transaction.pragma_update(None, "user_version", CACHE_SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(connection)
}

fn next_access(connection: &Connection) -> Result<i64, CacheError> {
    connection
        .query_row(
            "UPDATE cache_metadata
             SET access_counter = access_counter + 1
             WHERE singleton = 1
             RETURNING access_counter",
            [],
            |row| row.get(0),
        )
        .map_err(CacheError::from)
}

fn remove_database_files(path: &Path) -> Result<(), CacheError> {
    for candidate in [
        path.to_path_buf(),
        database_sidecar(path, "-wal"),
        database_sidecar(path, "-shm"),
    ] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CacheError::Io {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn database_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn stamp(path: &Path) -> Result<DependencyStamp, CacheError> {
    let shown = path.to_str().ok_or_else(|| CacheError::NonUtf8Path {
        path: path.to_path_buf(),
    })?;
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DependencyStamp {
                path: shown.to_owned(),
                kind: DependencyKind::Missing,
                digest: String::new(),
            });
        }
        Err(source) => {
            return Err(CacheError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.is_file() {
        let bytes = std::fs::read(path).map_err(|source| CacheError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        return Ok(DependencyStamp {
            path: shown.to_owned(),
            kind: DependencyKind::File,
            digest: format!("{:x}", Sha256::digest(bytes)),
        });
    }
    if metadata.is_dir() {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path).map_err(|source| CacheError::Io {
            path: path.to_path_buf(),
            source,
        })? {
            let entry = entry.map_err(|source| CacheError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| CacheError::Io {
                path: entry.path(),
                source,
            })?;
            entries.push((
                os_name_bytes(&entry.file_name()),
                file_type.is_file(),
                file_type.is_dir(),
                file_type.is_symlink(),
            ));
        }
        entries.sort();
        return Ok(DependencyStamp {
            path: shown.to_owned(),
            kind: DependencyKind::Directory,
            digest: format!("{:x}", Sha256::digest(serde_json::to_vec(&entries)?)),
        });
    }
    Ok(DependencyStamp {
        path: shown.to_owned(),
        kind: DependencyKind::Other,
        digest: String::new(),
    })
}

#[cfg(unix)]
fn os_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    name.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;
    name.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
fn os_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    name.to_string_lossy().as_bytes().to_vec()
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid cached JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cache dependency path is not valid UTF-8: {path}")]
    NonUtf8Path { path: PathBuf },
}

impl CacheError {
    fn is_contention(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(error, _))
                if matches!(error.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
        )
    }

    fn is_corruption(&self) -> bool {
        match self {
            Self::Sqlite(rusqlite::Error::SqliteFailure(error, _)) => matches!(
                error.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ),
            Self::Json(_) => true,
            Self::Io { .. } | Self::Sqlite(_) | Self::NonUtf8Path { .. } => false,
        }
    }
}
