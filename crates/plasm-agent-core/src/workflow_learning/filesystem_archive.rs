//! Bounded filesystem storage for portable teaching archive records.
//!
//! The filesystem codec owns object integrity and publication, not repository
//! authorization or adoption. Callers must validate the typed records and adopt
//! them in a transaction before making anything visible.
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

// The archive may be attacker-controlled. Open children relative to already
// opened directory descriptors and do not follow symlinks. Publish using the
// platform's atomic no-replace rename; unsupported platforms fail closed.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use std::ffi::{c_char, c_int, c_uint};
    use std::{
        ffi::CString,
        fs::{File, OpenOptions},
        io,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        },
        path::Path,
    };
    #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
    const DIRECTORY: c_int = 0o200000;
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    const DIRECTORY: c_int = 0o40000;
    #[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
    const NOFOLLOW: c_int = 0o400000;
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    const NOFOLLOW: c_int = 0o100000;
    #[cfg(target_os = "linux")]
    const CLOEXEC: c_int = 0o2000000;
    #[cfg(target_os = "macos")]
    const DIRECTORY: c_int = 0x00100000;
    #[cfg(target_os = "macos")]
    const NOFOLLOW: c_int = 0x100;
    #[cfg(target_os = "macos")]
    const CLOEXEC: c_int = 0x01000000;
    unsafe extern "C" {
        fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int, ...) -> c_int;
        #[cfg(target_os = "linux")]
        fn renameat2(
            olddirfd: c_int,
            oldpath: *const c_char,
            newdirfd: c_int,
            newpath: *const c_char,
            flags: c_uint,
        ) -> c_int;
        #[cfg(target_os = "macos")]
        fn renamex_np(oldpath: *const c_char, newpath: *const c_char, flags: c_uint) -> c_int;
    }
    fn cstring(bytes: &[u8]) -> io::Result<CString> {
        CString::new(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in archive path"))
    }
    pub fn root(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(DIRECTORY | NOFOLLOW | CLOEXEC)
            .open(path)
    }
    pub fn child(parent: &File, name: &str, directory: bool) -> io::Result<File> {
        let name = cstring(name.as_bytes())?;
        let flags = NOFOLLOW | CLOEXEC | if directory { DIRECTORY } else { 0 };
        // SAFETY: `name` is a valid NUL-terminated single path component and
        // `parent` owns an open directory descriptor for the duration of openat.
        let fd = unsafe { openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returns a new, uniquely owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        let ty = file.metadata()?.file_type();
        if if directory {
            !ty.is_dir()
        } else {
            !ty.is_file()
        } {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid archive entry type",
            ));
        }
        Ok(file)
    }
    pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
        let from = cstring(from.as_os_str().as_bytes())?;
        let to = cstring(to.as_os_str().as_bytes())?;
        // SAFETY: both pointers refer to valid NUL-terminated path strings; the
        // flag requires the OS to atomically reject any existing destination.
        #[cfg(target_os = "linux")]
        let result = unsafe { renameat2(-100, from.as_ptr(), -100, to.as_ptr(), 1) };
        #[cfg(target_os = "macos")]
        let result = unsafe { renamex_np(from.as_ptr(), to.as_ptr(), 4) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use std::{fs::File, io, path::Path};
    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "secure filesystem archives unsupported on this platform",
        )
    }
    pub fn root(_: &Path) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn child(_: &File, _: &str, _: bool) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn rename_noreplace(_: &Path, _: &Path) -> io::Result<()> {
        Err(unsupported())
    }
}

pub const FORMAT_VERSION: u32 = 2;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_RECORDS: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum FilesystemArchiveError {
    #[error("archive filesystem: {0}")]
    Io(#[from] io::Error),
    #[error("archive JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("archive database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("archive repository: {0}")]
    Repository(#[from] super::repository::RepositoryError),
    #[error("unsupported archive version")]
    Version,
    #[error("archive exceeds its bounded limits")]
    Limit,
    #[error("invalid archive manifest or object")]
    Invalid,
    #[error("archive object integrity check failed")]
    Integrity,
    #[error("archive destination already exists")]
    DestinationExists,
    #[error("archive was published but parent-directory durability could not be confirmed: {0}")]
    PublishedDurabilityUncertain(#[source] io::Error),
}
type Result<T> = std::result::Result<T, FilesystemArchiveError>;

/// The archive contains no execution results or application-specific grades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Observation,
    Execution,
    Review,
    SelectedReview,
    Example,
    CatalogueGuidance,
    ExampleSupport,
    GuidanceSupport,
    ExternalDecision,
    SharedContract,
}

/// Portable domain records. Execution results and host-specific grading have no
/// representation in this format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Record {
    Observation {
        source_scope: super::model::LibraryScope,
        observation: super::model::ProgramEvidence,
    },
    Execution {
        source_scope: super::model::LibraryScope,
        logical_session: super::model::LogicalSessionId,
        session_revision: u64,
        execution_id: super::model::ExecutionId,
        observation_digest: super::model::ReceiptDigest,
        fact: super::model::StoredExecutionFact,
        training_epoch: Option<Uuid>,
    },
    Review {
        review_id: Uuid,
        original_digest: super::archive::ArchiveDigest,
        content_digest: super::model::ReceiptDigest,
        disposition: StoredReviewState,
        seal: super::model::StoredReviewSeal,
    },
    /// Selected facts only, never a rewritten representation of the original
    /// review or its free-text claims. Source identity remains separate.
    SelectedReview {
        review_id: Uuid,
        original_digest: super::archive::ArchiveDigest,
        evidence: SelectedReviewEvidence,
    },
    Example {
        revision_id: Uuid,
        candidate: Box<super::model::Candidate>,
        state: StoredExampleState,
        reason: Option<String>,
    },
    CatalogueGuidance {
        association_id: Uuid,
        association: super::catalogue_guidance::CatalogueAssociation,
    },
    ExampleSupport {
        example_id: Uuid,
        evidence: super::archive::EvidenceReference,
    },
    GuidanceSupport {
        association_id: Uuid,
        evidence: super::archive::EvidenceReference,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredExampleState {
    Active,
    Candidate,
    Superseded,
    Bad,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredReviewState {
    Queued,
    Leased,
    Completed,
    Failed,
    AwaitingApproval,
}
impl TryFrom<&str> for StoredReviewState {
    type Error = FilesystemArchiveError;
    fn try_from(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "leased" => Ok(Self::Leased),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "awaiting_approval" => Ok(Self::AwaitingApproval),
            _ => Err(FilesystemArchiveError::Invalid),
        }
    }
}
impl TryFrom<&str> for StoredExampleState {
    type Error = FilesystemArchiveError;
    fn try_from(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "candidate" => Ok(Self::Candidate),
            "superseded" => Ok(Self::Superseded),
            "bad" => Ok(Self::Bad),
            _ => Err(FilesystemArchiveError::Invalid),
        }
    }
}
impl Record {
    fn kind(&self) -> RecordKind {
        match self {
            Self::Observation { .. } => RecordKind::Observation,
            Self::Execution { .. } => RecordKind::Execution,
            Self::Review { .. } => RecordKind::Review,
            Self::SelectedReview { .. } => RecordKind::SelectedReview,
            Self::Example { .. } => RecordKind::Example,
            Self::CatalogueGuidance { .. } => RecordKind::CatalogueGuidance,
            Self::ExampleSupport { .. } => RecordKind::ExampleSupport,
            Self::GuidanceSupport { .. } => RecordKind::GuidanceSupport,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedReviewEvidence {
    pub source_scope: super::model::LibraryScope,
    pub logical_session: super::model::LogicalSessionId,
    pub session_revision: u64,
    pub assurance: super::model::OutcomeAssurance,
    pub executions: Vec<super::model::ExecutionReceiptRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRef {
    pub kind: RecordKind,
    pub digest: String,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub kind: super::archive::ArchiveKind,
    pub source_scope: super::model::LibraryScope,
    pub records: BTreeMap<String, RecordRef>,
    pub digest: String,
}

/// Host policy fixes the authorized source and expected archive identity. Task
/// exclusion, grading and evaluation partitions are harness responsibilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPolicy {
    pub expected_digest: super::archive::ArchiveDigest,
    pub source_scope: super::model::LibraryScope,
}

/// The authenticated adapter supplies the actor; wire arguments cannot claim
/// any authority. The external decision binds one exact imported review.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedReviewAttestationRequest {
    pub archive_digest: super::archive::ArchiveDigest,
    pub evidence: super::archive::EvidenceReference,
    pub decision_id: Uuid,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Source identity is explicit even when imported evidence is re-exported from
/// a different target project. The token is only a path component, not authority.
pub fn scope_token(scope: &super::model::LibraryScope) -> Result<String> {
    Ok(digest(&canonical(scope)?))
}
fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    fn sort(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(sort).collect())
            }
            serde_json::Value::Object(values) => {
                let sorted: BTreeMap<_, _> = values
                    .into_iter()
                    .map(|(key, value)| (key, sort(value)))
                    .collect();
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            other => other,
        }
    }
    Ok(serde_json::to_vec(&sort(serde_json::to_value(value)?))?)
}
pub(super) fn record_digest(record: &Record) -> Result<String> {
    Ok(digest(&canonical(record)?))
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn manifest_digest(
    kind: super::archive::ArchiveKind,
    scope: &super::model::LibraryScope,
    records: &BTreeMap<String, RecordRef>,
) -> Result<String> {
    Ok(digest(&canonical(&(FORMAT_VERSION, kind, scope, records))?))
}
fn object_path(root: &Path, hash: &str) -> Result<PathBuf> {
    if !valid_digest(hash) {
        return Err(FilesystemArchiveError::Invalid);
    }
    Ok(root.join("objects").join(&hash[..2]).join(hash))
}
fn regular_file(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(FilesystemArchiveError::Invalid);
    }
    Ok(File::open(path)?)
}
fn regular_directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(FilesystemArchiveError::Invalid);
    }
    Ok(())
}

/// A new archive is staged beside its destination until its manifest is durable.
pub struct Writer {
    destination: PathBuf,
    staging: PathBuf,
    scope: super::model::LibraryScope,
    kind: super::archive::ArchiveKind,
    records: BTreeMap<String, RecordRef>,
    published: bool,
}
impl Writer {
    pub fn new(
        destination: impl AsRef<Path>,
        scope: super::model::LibraryScope,
        kind: super::archive::ArchiveKind,
    ) -> Result<Self> {
        let destination = destination.as_ref().to_path_buf();
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(FilesystemArchiveError::DestinationExists);
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        regular_directory(parent)?;
        let staging = parent.join(format!(".plasm-archive-{}", Uuid::new_v4()));
        let mut directory = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(&staging)?;
        fs::create_dir(staging.join("objects"))?;
        Ok(Self {
            destination,
            staging,
            scope,
            kind,
            records: BTreeMap::new(),
            published: false,
        })
    }

    /// Writes one bounded JSON record, deduplicating identical serialized bytes.
    pub fn write_record(&mut self, identity: &str, record: &Record) -> Result<()> {
        self.write(identity, record.kind(), record)
    }

    fn write<T: Serialize>(&mut self, identity: &str, kind: RecordKind, record: &T) -> Result<()> {
        if identity.trim().is_empty() || identity.len() > 512 || self.records.len() >= MAX_RECORDS {
            return Err(FilesystemArchiveError::Limit);
        }
        let bytes = canonical(record)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(FilesystemArchiveError::Limit);
        }
        let hash = digest(&bytes);
        if let Some(existing) = self.records.get(identity) {
            return if existing.kind == kind
                && existing.digest == hash
                && existing.size == bytes.len() as u64
            {
                Ok(())
            } else {
                Err(FilesystemArchiveError::Invalid)
            };
        }
        let path = object_path(&self.staging, &hash)?;
        let directory = path.parent().ok_or(FilesystemArchiveError::Invalid)?;
        fs::create_dir_all(directory)?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(&bytes)?;
                file.sync_all()?;
                File::open(directory)?.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let mut existing = Vec::new();
                regular_file(&path)?
                    .take((MAX_RECORD_BYTES + 1) as u64)
                    .read_to_end(&mut existing)?;
                if existing != bytes {
                    return Err(FilesystemArchiveError::Integrity);
                }
            }
            Err(error) => return Err(error.into()),
        }
        self.records.insert(
            identity.to_owned(),
            RecordRef {
                kind,
                digest: hash,
                size: bytes.len() as u64,
            },
        );
        Ok(())
    }

    pub fn finish(mut self) -> Result<Manifest> {
        let manifest = Manifest {
            version: FORMAT_VERSION,
            kind: self.kind,
            digest: manifest_digest(self.kind, &self.scope, &self.records)?,
            source_scope: self.scope.clone(),
            records: std::mem::take(&mut self.records),
        };
        let bytes = serde_json::to_vec(&manifest)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(FilesystemArchiveError::Limit);
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.staging.join("manifest.json"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        File::open(self.staging.join("objects"))?.sync_all()?;
        File::open(&self.staging)?.sync_all()?;
        if let Err(error) = platform::rename_noreplace(&self.staging, &self.destination) {
            if error.kind() == io::ErrorKind::AlreadyExists {
                return Err(FilesystemArchiveError::DestinationExists);
            }
            return Err(error.into());
        }
        self.published = true;
        let parent = self
            .destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(FilesystemArchiveError::PublishedDurabilityUncertain)?;
        Ok(manifest)
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.staging);
        }
    }
}

pub struct Reader {
    objects: File,
    manifest: Manifest,
}
impl Reader {
    /// Verify all declared objects before allowing an adoption transaction to begin.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = platform::root(root.as_ref())?;
        let objects = platform::child(&root, "objects", true)?;
        let mut bytes = Vec::new();
        platform::child(&root, "manifest.json", false)?
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(FilesystemArchiveError::Limit);
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        if manifest.version != FORMAT_VERSION {
            return Err(FilesystemArchiveError::Version);
        }
        if manifest.records.len() > MAX_RECORDS {
            return Err(FilesystemArchiveError::Limit);
        }
        if manifest.digest
            != manifest_digest(manifest.kind, &manifest.source_scope, &manifest.records)?
        {
            return Err(FilesystemArchiveError::Integrity);
        }
        let reader = Self { objects, manifest };
        for record in reader.manifest.records.values() {
            reader.read_bytes(record)?;
        }
        Ok(reader)
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn into_manifest(self) -> Manifest {
        self.manifest
    }
    fn read_bytes(&self, reference: &RecordRef) -> Result<Vec<u8>> {
        if reference.size > MAX_RECORD_BYTES as u64 {
            return Err(FilesystemArchiveError::Limit);
        }
        if !valid_digest(&reference.digest) {
            return Err(FilesystemArchiveError::Invalid);
        }
        let prefix = platform::child(&self.objects, &reference.digest[..2], true)?;
        let mut bytes = Vec::new();
        platform::child(&prefix, &reference.digest, false)?
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != reference.size || digest(&bytes) != reference.digest {
            return Err(FilesystemArchiveError::Integrity);
        }
        Ok(bytes)
    }
    /// Re-verification protects against changes between initial validation and use.
    pub fn read<T: DeserializeOwned>(&self, identity: &str) -> Result<T> {
        let reference = self
            .manifest
            .records
            .get(identity)
            .ok_or(FilesystemArchiveError::Invalid)?;
        Ok(serde_json::from_slice(&self.read_bytes(reference)?)?)
    }
    pub fn read_record(&self, identity: &str) -> Result<Record> {
        let reference = self
            .manifest
            .records
            .get(identity)
            .ok_or(FilesystemArchiveError::Invalid)?;
        let record: Record = serde_json::from_slice(&self.read_bytes(reference)?)?;
        if record.kind() != reference.kind {
            return Err(FilesystemArchiveError::Invalid);
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!("plasm-archive-test-{}", Uuid::new_v4()))
    }
    fn scope() -> super::super::model::LibraryScope {
        super::super::model::LibraryScope {
            tenant: "tenant".into(),
            project: "project".into(),
        }
    }

    #[test]
    fn identical_records_share_an_object_and_round_trip() {
        let path = root();
        let mut writer = Writer::new(
            &path,
            scope(),
            super::super::archive::ArchiveKind::PrivateCollection,
        )
        .unwrap();
        let value = serde_json::json!({"source":"class Example(Program): pass"});
        writer
            .write("first", RecordKind::Observation, &value)
            .unwrap();
        writer
            .write("second", RecordKind::Observation, &value)
            .unwrap();
        let manifest = writer.finish().unwrap();
        assert_eq!(
            manifest.records["first"].digest,
            manifest.records["second"].digest
        );
        let reader = Reader::open(&path).unwrap();
        assert_eq!(reader.read::<serde_json::Value>("first").unwrap(), value);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_modified_objects_and_manifest() {
        let path = root();
        let mut writer = Writer::new(
            &path,
            scope(),
            super::super::archive::ArchiveKind::PrivateCollection,
        )
        .unwrap();
        writer
            .write(
                "program",
                RecordKind::Observation,
                &serde_json::json!({"source":"x"}),
            )
            .unwrap();
        let manifest = writer.finish().unwrap();
        let object = object_path(&path, &manifest.records["program"].digest).unwrap();
        fs::write(object, b"changed").unwrap();
        assert!(matches!(
            Reader::open(&path),
            Err(FilesystemArchiveError::Integrity)
        ));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn aborted_writer_does_not_publish_and_existing_destination_is_preserved() {
        let path = root();
        let mut writer = Writer::new(
            &path,
            scope(),
            super::super::archive::ArchiveKind::PrivateCollection,
        )
        .unwrap();
        writer
            .write("program", RecordKind::Observation, &"source")
            .unwrap();
        drop(writer);
        assert!(!path.exists());
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"keep").unwrap();
        assert!(matches!(
            Writer::new(
                &path,
                scope(),
                super::super::archive::ArchiveKind::PrivateCollection
            ),
            Err(FilesystemArchiveError::DestinationExists)
        ));
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"keep");
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn destination_created_after_staging_is_never_replaced() {
        let path = root();
        let writer = Writer::new(
            &path,
            scope(),
            super::super::archive::ArchiveKind::PrivateCollection,
        )
        .unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            writer.finish(),
            Err(FilesystemArchiveError::DestinationExists)
        ));
        assert!(path.is_dir());
        fs::remove_dir_all(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_object_and_symlink_prefix() {
        use std::os::unix::fs::symlink;
        let path = root();
        let mut writer = Writer::new(
            &path,
            scope(),
            super::super::archive::ArchiveKind::PrivateCollection,
        )
        .unwrap();
        writer
            .write("program", RecordKind::Observation, &"source")
            .unwrap();
        let manifest = writer.finish().unwrap();
        let object = object_path(&path, &manifest.records["program"].digest).unwrap();
        let contents = fs::read(&object).unwrap();
        let outside = root();
        fs::write(&outside, contents).unwrap();
        fs::remove_file(&object).unwrap();
        symlink(&outside, &object).unwrap();
        assert!(Reader::open(&path).is_err());
        fs::remove_dir_all(path).unwrap();
        fs::remove_file(outside).unwrap();
    }
}
