//! Agent-private browser transfer staging.
//!
//! Browser transfer bytes cross the signed Hub↔Agent boundary, but local paths do not.
//! Uploads are materialized below an Agent-private root and referenced by random handles.
//! Downloads are written by Cua into an Agent-private per-operation directory, revalidated,
//! bounded, and returned as bytes plus an opaque handle. Every handle is fenced by the
//! interaction context, device generation, and capability revision.

use crate::v2_browser::{
    MAX_BROWSER_DOWNLOAD_BYTES, MAX_BROWSER_DOWNLOAD_FILES, MAX_BROWSER_UPLOAD_FILE_BYTES,
    MAX_BROWSER_UPLOAD_FILES, validate_download_destination_name, validate_upload_file_name,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rand::{RngCore, rngs::OsRng};
use std::{
    collections::HashMap,
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserStagingStartupStage {
    RemoveExistingRoot,
    CreateRoot,
    SetPermissions,
    Metadata,
    Canonicalize,
}

impl BrowserStagingStartupStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RemoveExistingRoot => "remove_existing_root",
            Self::CreateRoot => "create_root",
            Self::SetPermissions => "set_permissions",
            Self::Metadata => "metadata",
            Self::Canonicalize => "canonicalize",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserStagingStartupFailure {
    Io,
    InvalidRoot,
}

impl BrowserStagingStartupFailure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::InvalidRoot => "invalid_root",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserStagingStartupError {
    stage: BrowserStagingStartupStage,
    failure: BrowserStagingStartupFailure,
    io_kind: Option<std::io::ErrorKind>,
}

impl BrowserStagingStartupError {
    fn from_private(stage: BrowserStagingStartupStage, error: PrivateRootError) -> Self {
        match error {
            PrivateRootError::InvalidRoot => Self::invalid_root(stage),
            PrivateRootError::Io(kind) => Self {
                stage,
                failure: BrowserStagingStartupFailure::Io,
                io_kind: Some(kind),
            },
        }
    }

    fn io(stage: BrowserStagingStartupStage, error: &std::io::Error) -> Self {
        Self {
            stage,
            failure: BrowserStagingStartupFailure::Io,
            io_kind: Some(error.kind()),
        }
    }

    fn invalid_root(stage: BrowserStagingStartupStage) -> Self {
        Self {
            stage,
            failure: BrowserStagingStartupFailure::InvalidRoot,
            io_kind: None,
        }
    }

    pub const fn stage(&self) -> &'static str {
        self.stage.as_str()
    }

    pub const fn failure_class(&self) -> &'static str {
        self.failure.as_str()
    }

    pub fn io_class(&self) -> &'static str {
        match self.io_kind {
            Some(std::io::ErrorKind::NotFound) => "not_found",
            Some(std::io::ErrorKind::PermissionDenied) => "permission_denied",
            Some(std::io::ErrorKind::AlreadyExists) => "already_exists",
            Some(std::io::ErrorKind::WouldBlock) => "would_block",
            Some(std::io::ErrorKind::InvalidInput) => "invalid_input",
            Some(std::io::ErrorKind::InvalidData) => "invalid_data",
            Some(std::io::ErrorKind::TimedOut) => "timed_out",
            Some(std::io::ErrorKind::Interrupted) => "interrupted",
            Some(std::io::ErrorKind::WriteZero) => "write_zero",
            Some(std::io::ErrorKind::UnexpectedEof) => "unexpected_eof",
            Some(std::io::ErrorKind::OutOfMemory) => "out_of_memory",
            Some(std::io::ErrorKind::StorageFull) => "storage_full",
            Some(_) => "other",
            None => "not_applicable",
        }
    }

    pub const fn is_io(&self) -> bool {
        matches!(self.failure, BrowserStagingStartupFailure::Io)
    }

    pub const fn upload_safe_error_code(&self) -> &'static str {
        if self.is_io() {
            "browser_upload_staging_io"
        } else {
            "browser_upload_staging_invalid_root"
        }
    }

    pub const fn download_safe_error_code(&self) -> &'static str {
        if self.is_io() {
            "browser_download_staging_io"
        } else {
            "browser_download_staging_invalid_root"
        }
    }
}

impl fmt::Display for BrowserStagingStartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("browser_staging_startup_failed")
    }
}

impl std::error::Error for BrowserStagingStartupError {}

#[derive(Clone)]
pub struct BrowserUploadStagingBroker {
    inner: Arc<Mutex<UploadInner>>,
}

struct UploadInner {
    root: PathBuf,
    canonical_root: PathBuf,
    records: HashMap<String, StagedUploadRecord>,
}

struct StagedUploadRecord {
    context_id: String,
    device_generation: u64,
    capability_revision: u64,
    canonical_path: PathBuf,
    bytes: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedStagedUpload {
    canonical_path: String,
}

impl ResolvedStagedUpload {
    pub(crate) fn canonical_path(&self) -> &str {
        &self.canonical_path
    }
}

impl fmt::Debug for ResolvedStagedUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedStagedUpload")
            .field("canonical_path", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedUploadHandle {
    pub handle: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserUploadStagingError {
    InvalidRoot,
    InvalidFileName,
    InvalidData,
    FileTooLarge,
    FileLimitExceeded,
    UnknownHandle,
    ContextMismatch,
    GenerationMismatch,
    CapabilityRevisionMismatch,
    InvalidStagedFile,
    Io,
}

impl fmt::Display for BrowserUploadStagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BrowserUploadStagingError {}

impl BrowserUploadStagingError {
    pub const fn safe_error_code(&self) -> &'static str {
        match self {
            Self::InvalidRoot => "browser_upload_staging_invalid_root",
            Self::InvalidFileName => "browser_upload_invalid_name",
            Self::InvalidData => "browser_upload_invalid_data",
            Self::FileTooLarge => "browser_upload_file_too_large",
            Self::FileLimitExceeded => "browser_upload_file_limit",
            Self::UnknownHandle => "browser_upload_ref_stale",
            Self::ContextMismatch => "browser_upload_ref_context_mismatch",
            Self::GenerationMismatch => "browser_upload_ref_generation_mismatch",
            Self::CapabilityRevisionMismatch => "browser_upload_ref_revision_mismatch",
            Self::InvalidStagedFile => "browser_upload_staged_file_invalid",
            Self::Io => "browser_upload_staging_io",
        }
    }
}

impl BrowserUploadStagingBroker {
    pub fn new(state_dir: &Path) -> Result<Self, BrowserStagingStartupError> {
        let (root, canonical_root) = create_private_root(state_dir, "browser-upload-staging")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(UploadInner {
                root,
                canonical_root,
                records: HashMap::new(),
            })),
        })
    }

    pub fn stage(
        &self,
        context_id: &str,
        device_generation: u64,
        capability_revision: u64,
        file_name: &str,
        data_base64: &str,
        expected_bytes: u64,
    ) -> Result<StagedUploadHandle, BrowserUploadStagingError> {
        validate_upload_file_name(file_name)
            .map_err(|_| BrowserUploadStagingError::InvalidFileName)?;
        if expected_bytes > MAX_BROWSER_UPLOAD_FILE_BYTES as u64 {
            return Err(BrowserUploadStagingError::FileTooLarge);
        }
        let bytes = STANDARD
            .decode(data_base64.as_bytes())
            .map_err(|_| BrowserUploadStagingError::InvalidData)?;
        if bytes.len() > MAX_BROWSER_UPLOAD_FILE_BYTES
            || u64::try_from(bytes.len()).ok() != Some(expected_bytes)
        {
            return Err(BrowserUploadStagingError::InvalidData);
        }

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserUploadStagingError::Io)?;
        if inner
            .records
            .values()
            .filter(|record| record.context_id == context_id)
            .count()
            >= MAX_BROWSER_UPLOAD_FILES
        {
            return Err(BrowserUploadStagingError::FileLimitExceeded);
        }

        let handle = unique_handle("upload_", |candidate| inner.records.contains_key(candidate))
            .map_err(|_| BrowserUploadStagingError::Io)?;
        let directory = inner.root.join(&handle);
        fs::create_dir(&directory).map_err(|_| BrowserUploadStagingError::Io)?;
        harden_directory_permissions(&directory).map_err(|_| BrowserUploadStagingError::Io)?;
        let path = directory.join(file_name);
        let write_result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            harden_file_open_options(&mut options);
            let mut file = options
                .open(&path)
                .map_err(|_| BrowserUploadStagingError::Io)?;
            file.write_all(&bytes)
                .map_err(|_| BrowserUploadStagingError::Io)?;
            file.sync_all().map_err(|_| BrowserUploadStagingError::Io)?;
            prove_direct_regular_file(&path, &inner.canonical_root, expected_bytes, expected_bytes)
                .map_err(|_| BrowserUploadStagingError::InvalidStagedFile)
        })();
        let canonical_path = match write_result {
            Ok(path) => path,
            Err(error) => {
                let _ = fs::remove_dir_all(&directory);
                return Err(error);
            }
        };
        inner.records.insert(
            handle.clone(),
            StagedUploadRecord {
                context_id: context_id.to_owned(),
                device_generation,
                capability_revision,
                canonical_path,
                bytes: expected_bytes,
            },
        );
        Ok(StagedUploadHandle {
            handle,
            bytes: expected_bytes,
        })
    }

    pub fn resolve(
        &self,
        handle: &str,
        context_id: &str,
        device_generation: u64,
        capability_revision: u64,
    ) -> Result<ResolvedStagedUpload, BrowserUploadStagingError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| BrowserUploadStagingError::Io)?;
        let record = inner
            .records
            .get(handle)
            .ok_or(BrowserUploadStagingError::UnknownHandle)?;
        validate_upload_record(record, context_id, device_generation, capability_revision)?;
        let canonical_path = prove_direct_regular_file(
            &record.canonical_path,
            &inner.canonical_root,
            record.bytes,
            MAX_BROWSER_UPLOAD_FILE_BYTES as u64,
        )
        .map_err(|_| BrowserUploadStagingError::InvalidStagedFile)?;
        let canonical_path = canonical_path
            .to_str()
            .ok_or(BrowserUploadStagingError::InvalidStagedFile)?
            .to_owned();
        Ok(ResolvedStagedUpload { canonical_path })
    }

    pub fn consume_handles(
        &self,
        handles: &[String],
        context_id: &str,
        device_generation: u64,
        capability_revision: u64,
    ) -> Result<(), BrowserUploadStagingError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserUploadStagingError::Io)?;
        for handle in handles {
            let record = inner
                .records
                .get(handle)
                .ok_or(BrowserUploadStagingError::UnknownHandle)?;
            validate_upload_record(record, context_id, device_generation, capability_revision)?;
        }
        for handle in handles {
            if let Some(record) = inner.records.remove(handle)
                && let Some(directory) = record.canonical_path.parent()
            {
                let _ = fs::remove_dir_all(directory);
            }
        }
        Ok(())
    }

    pub fn cleanup_context(&self, context_id: &str) -> Result<(), BrowserUploadStagingError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserUploadStagingError::Io)?;
        let handles: Vec<_> = inner
            .records
            .iter()
            .filter(|(_, record)| record.context_id == context_id)
            .map(|(handle, _)| handle.clone())
            .collect();
        for handle in handles {
            if let Some(record) = inner.records.remove(&handle)
                && let Some(directory) = record.canonical_path.parent()
            {
                let _ = fs::remove_dir_all(directory);
            }
        }
        Ok(())
    }

    pub fn cleanup_all(&self) -> Result<(), BrowserUploadStagingError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserUploadStagingError::Io)?;
        for record in inner.records.values() {
            if let Some(directory) = record.canonical_path.parent() {
                let _ = fs::remove_dir_all(directory);
            }
        }
        inner.records.clear();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.records.len())
            .unwrap_or(0)
    }
}

fn validate_upload_record(
    record: &StagedUploadRecord,
    context_id: &str,
    device_generation: u64,
    capability_revision: u64,
) -> Result<(), BrowserUploadStagingError> {
    if record.context_id != context_id {
        return Err(BrowserUploadStagingError::ContextMismatch);
    }
    if record.device_generation != device_generation {
        return Err(BrowserUploadStagingError::GenerationMismatch);
    }
    if record.capability_revision != capability_revision {
        return Err(BrowserUploadStagingError::CapabilityRevisionMismatch);
    }
    Ok(())
}

impl Drop for UploadInner {
    fn drop(&mut self) {
        let _ = remove_private_root(&self.root);
    }
}

#[derive(Clone)]
pub struct BrowserDownloadStagingBroker {
    inner: Arc<Mutex<DownloadInner>>,
}

struct DownloadInner {
    root: PathBuf,
    canonical_root: PathBuf,
    pending: HashMap<String, PendingDownload>,
    completed: HashMap<String, CompletedDownload>,
}

#[derive(Clone)]
struct PendingDownload {
    context_id: String,
    device_generation: u64,
    capability_revision: u64,
    directory: PathBuf,
    destination_name: String,
    max_bytes: u64,
    overwrite: bool,
}

struct CompletedDownload {
    context_id: String,
    directory: PathBuf,
    destination_name: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PreparedBrowserDownload {
    operation_handle: String,
    canonical_root: String,
}

impl PreparedBrowserDownload {
    pub(crate) fn operation_handle(&self) -> &str {
        &self.operation_handle
    }

    pub(crate) fn canonical_root(&self) -> &str {
        &self.canonical_root
    }
}

impl fmt::Debug for PreparedBrowserDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedBrowserDownload")
            .field("operation_handle", &"[redacted]")
            .field("canonical_root", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct FinalizedBrowserDownload {
    pub backend_download_handle: String,
    pub destination_name: String,
    pub bytes: u64,
    pub data_base64: String,
}

impl fmt::Debug for FinalizedBrowserDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FinalizedBrowserDownload")
            .field("backend_download_handle", &"[redacted]")
            .field("destination_name", &self.destination_name)
            .field("bytes", &self.bytes)
            .field("data_base64", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserDownloadStagingError {
    InvalidRoot,
    InvalidFileName,
    InvalidBound,
    FileLimitExceeded,
    Collision,
    UnknownOperation,
    ContextMismatch,
    GenerationMismatch,
    CapabilityRevisionMismatch,
    InvalidBackendId,
    InvalidCompletedFile,
    FileTooLarge,
    Io,
}

impl fmt::Display for BrowserDownloadStagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BrowserDownloadStagingError {}

impl BrowserDownloadStagingError {
    pub const fn safe_error_code(&self) -> &'static str {
        match self {
            Self::InvalidRoot => "browser_download_staging_invalid_root",
            Self::InvalidFileName => "browser_download_invalid_name",
            Self::InvalidBound => "browser_download_invalid_bound",
            Self::FileLimitExceeded => "browser_download_file_limit",
            Self::Collision => "browser_download_destination_collision",
            Self::UnknownOperation => "browser_download_operation_stale",
            Self::ContextMismatch => "browser_download_context_mismatch",
            Self::GenerationMismatch => "browser_download_generation_mismatch",
            Self::CapabilityRevisionMismatch => "browser_download_revision_mismatch",
            Self::InvalidBackendId => "browser_download_backend_id_invalid",
            Self::InvalidCompletedFile => "browser_download_completed_file_invalid",
            Self::FileTooLarge => "browser_download_file_too_large",
            Self::Io => "browser_download_staging_io",
        }
    }
}

impl BrowserDownloadStagingBroker {
    pub fn new(state_dir: &Path) -> Result<Self, BrowserStagingStartupError> {
        let (root, canonical_root) = create_private_root(state_dir, "browser-download-staging")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(DownloadInner {
                root,
                canonical_root,
                pending: HashMap::new(),
                completed: HashMap::new(),
            })),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &self,
        context_id: &str,
        device_generation: u64,
        capability_revision: u64,
        destination_name: &str,
        max_bytes: u64,
        overwrite: bool,
    ) -> Result<PreparedBrowserDownload, BrowserDownloadStagingError> {
        validate_download_destination_name(destination_name)
            .map_err(|_| BrowserDownloadStagingError::InvalidFileName)?;
        if max_bytes == 0 || max_bytes > MAX_BROWSER_DOWNLOAD_BYTES {
            return Err(BrowserDownloadStagingError::InvalidBound);
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserDownloadStagingError::Io)?;
        let active_count = inner
            .pending
            .values()
            .filter(|record| record.context_id == context_id)
            .count()
            + inner
                .completed
                .values()
                .filter(|record| record.context_id == context_id)
                .count();
        if active_count >= MAX_BROWSER_DOWNLOAD_FILES {
            return Err(BrowserDownloadStagingError::FileLimitExceeded);
        }
        if inner.pending.values().any(|record| {
            record.context_id == context_id && record.destination_name == destination_name
        }) {
            return Err(BrowserDownloadStagingError::Collision);
        }
        if !overwrite
            && inner.completed.values().any(|record| {
                record.context_id == context_id && record.destination_name == destination_name
            })
        {
            return Err(BrowserDownloadStagingError::Collision);
        }

        let operation_handle = unique_handle("download_op_", |candidate| {
            inner.pending.contains_key(candidate) || inner.completed.contains_key(candidate)
        })
        .map_err(|_| BrowserDownloadStagingError::Io)?;
        let directory = inner.root.join(&operation_handle);
        fs::create_dir(&directory).map_err(|_| BrowserDownloadStagingError::Io)?;
        harden_directory_permissions(&directory).map_err(|_| BrowserDownloadStagingError::Io)?;
        let canonical_directory =
            fs::canonicalize(&directory).map_err(|_| BrowserDownloadStagingError::InvalidRoot)?;
        if canonical_directory.parent() != Some(&inner.canonical_root) {
            let _ = fs::remove_dir_all(&directory);
            return Err(BrowserDownloadStagingError::InvalidRoot);
        }
        let canonical_root = canonical_directory
            .to_str()
            .ok_or(BrowserDownloadStagingError::InvalidRoot)?
            .to_owned();
        inner.pending.insert(
            operation_handle.clone(),
            PendingDownload {
                context_id: context_id.to_owned(),
                device_generation,
                capability_revision,
                directory: canonical_directory,
                destination_name: destination_name.to_owned(),
                max_bytes,
                overwrite,
            },
        );
        Ok(PreparedBrowserDownload {
            operation_handle,
            canonical_root,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finalize(
        &self,
        operation_handle: &str,
        context_id: &str,
        device_generation: u64,
        capability_revision: u64,
        backend_download_id: &str,
        backend_bytes: u64,
    ) -> Result<FinalizedBrowserDownload, BrowserDownloadStagingError> {
        if !safe_single_component(backend_download_id) {
            self.abort_best_effort(operation_handle);
            return Err(BrowserDownloadStagingError::InvalidBackendId);
        }
        let pending = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| BrowserDownloadStagingError::Io)?;
            let pending = inner
                .pending
                .get(operation_handle)
                .ok_or(BrowserDownloadStagingError::UnknownOperation)?;
            validate_download_record(pending, context_id, device_generation, capability_revision)?;
            pending.clone()
        };
        if backend_bytes > pending.max_bytes || backend_bytes > MAX_BROWSER_DOWNLOAD_BYTES {
            self.abort_best_effort(operation_handle);
            return Err(BrowserDownloadStagingError::FileTooLarge);
        }
        let path = pending.directory.join(backend_download_id);
        let canonical_path = match prove_direct_regular_file(
            &path,
            &pending.directory,
            backend_bytes,
            pending.max_bytes,
        ) {
            Ok(path) if path.parent() == Some(pending.directory.as_path()) => path,
            _ => {
                self.abort_best_effort(operation_handle);
                return Err(BrowserDownloadStagingError::InvalidCompletedFile);
            }
        };
        let data = match read_bounded_exact(&canonical_path, pending.max_bytes, backend_bytes) {
            Ok(data) => data,
            Err(error) => {
                self.abort_best_effort(operation_handle);
                return Err(error);
            }
        };

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserDownloadStagingError::Io)?;
        let current = inner
            .pending
            .get(operation_handle)
            .ok_or(BrowserDownloadStagingError::UnknownOperation)?;
        validate_download_record(current, context_id, device_generation, capability_revision)?;
        let old_handles: Vec<String> = if pending.overwrite {
            inner
                .completed
                .iter()
                .filter(|(_, record)| {
                    record.context_id == context_id
                        && record.destination_name == pending.destination_name
                })
                .map(|(handle, _)| handle.clone())
                .collect()
        } else {
            Vec::new()
        };
        for handle in old_handles {
            if let Some(record) = inner.completed.remove(&handle) {
                let _ = fs::remove_dir_all(&record.directory);
            }
        }
        inner.pending.remove(operation_handle);
        let handle = unique_handle("download_", |candidate| {
            inner.pending.contains_key(candidate) || inner.completed.contains_key(candidate)
        })
        .map_err(|_| BrowserDownloadStagingError::Io)?;
        inner.completed.insert(
            handle.clone(),
            CompletedDownload {
                context_id: context_id.to_owned(),
                directory: pending.directory,
                destination_name: pending.destination_name.clone(),
            },
        );
        Ok(FinalizedBrowserDownload {
            backend_download_handle: handle,
            destination_name: pending.destination_name,
            bytes: backend_bytes,
            data_base64: STANDARD.encode(data),
        })
    }

    pub fn abort(
        &self,
        operation_handle: &str,
        context_id: &str,
        device_generation: u64,
        capability_revision: u64,
    ) -> Result<(), BrowserDownloadStagingError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserDownloadStagingError::Io)?;
        let pending = inner
            .pending
            .get(operation_handle)
            .ok_or(BrowserDownloadStagingError::UnknownOperation)?;
        validate_download_record(pending, context_id, device_generation, capability_revision)?;
        let pending = inner
            .pending
            .remove(operation_handle)
            .ok_or(BrowserDownloadStagingError::UnknownOperation)?;
        let _ = fs::remove_dir_all(pending.directory);
        Ok(())
    }

    fn abort_best_effort(&self, operation_handle: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some(pending) = inner.pending.remove(operation_handle) {
            let _ = fs::remove_dir_all(pending.directory);
        }
    }

    pub fn cleanup_context(&self, context_id: &str) -> Result<(), BrowserDownloadStagingError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserDownloadStagingError::Io)?;
        let pending: Vec<String> = inner
            .pending
            .iter()
            .filter(|(_, record)| record.context_id == context_id)
            .map(|(handle, _)| handle.clone())
            .collect();
        for handle in pending {
            if let Some(record) = inner.pending.remove(&handle) {
                let _ = fs::remove_dir_all(record.directory);
            }
        }
        let completed: Vec<String> = inner
            .completed
            .iter()
            .filter(|(_, record)| record.context_id == context_id)
            .map(|(handle, _)| handle.clone())
            .collect();
        for handle in completed {
            if let Some(record) = inner.completed.remove(&handle) {
                let _ = fs::remove_dir_all(record.directory);
            }
        }
        Ok(())
    }

    pub fn cleanup_all(&self) -> Result<(), BrowserDownloadStagingError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BrowserDownloadStagingError::Io)?;
        for record in inner.pending.values() {
            let _ = fs::remove_dir_all(&record.directory);
        }
        for record in inner.completed.values() {
            let _ = fs::remove_dir_all(&record.directory);
        }
        inner.pending.clear();
        inner.completed.clear();
        Ok(())
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.pending.len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn completed_len(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.completed.len())
            .unwrap_or(0)
    }
}

fn validate_download_record(
    record: &PendingDownload,
    context_id: &str,
    device_generation: u64,
    capability_revision: u64,
) -> Result<(), BrowserDownloadStagingError> {
    if record.context_id != context_id {
        return Err(BrowserDownloadStagingError::ContextMismatch);
    }
    if record.device_generation != device_generation {
        return Err(BrowserDownloadStagingError::GenerationMismatch);
    }
    if record.capability_revision != capability_revision {
        return Err(BrowserDownloadStagingError::CapabilityRevisionMismatch);
    }
    Ok(())
}

impl Drop for DownloadInner {
    fn drop(&mut self) {
        let _ = remove_private_root(&self.root);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivateRootError {
    InvalidRoot,
    Io(std::io::ErrorKind),
}

fn create_private_root(
    state_dir: &Path,
    child: &str,
) -> Result<(PathBuf, PathBuf), BrowserStagingStartupError> {
    create_private_root_inner(state_dir, child, None)
}

fn create_private_root_inner(
    state_dir: &Path,
    child: &str,
    injected_failure: Option<(BrowserStagingStartupStage, std::io::ErrorKind)>,
) -> Result<(PathBuf, PathBuf), BrowserStagingStartupError> {
    let root = state_dir.join(child);
    maybe_inject_startup_failure(
        injected_failure,
        BrowserStagingStartupStage::RemoveExistingRoot,
    )?;
    remove_private_root(&root).map_err(|error| {
        BrowserStagingStartupError::from_private(
            BrowserStagingStartupStage::RemoveExistingRoot,
            error,
        )
    })?;
    maybe_inject_startup_failure(injected_failure, BrowserStagingStartupStage::CreateRoot)?;
    fs::create_dir_all(&root).map_err(|error| {
        BrowserStagingStartupError::io(BrowserStagingStartupStage::CreateRoot, &error)
    })?;
    maybe_inject_startup_failure(injected_failure, BrowserStagingStartupStage::SetPermissions)?;
    harden_directory_permissions(&root).map_err(|error| {
        BrowserStagingStartupError::io(BrowserStagingStartupStage::SetPermissions, &error)
    })?;
    maybe_inject_startup_failure(injected_failure, BrowserStagingStartupStage::Metadata)?;
    let metadata = fs::symlink_metadata(&root).map_err(|error| {
        BrowserStagingStartupError::io(BrowserStagingStartupStage::Metadata, &error)
    })?;
    validate_private_root_metadata(&metadata)?;
    maybe_inject_startup_failure(injected_failure, BrowserStagingStartupStage::Canonicalize)?;
    let canonical_root = fs::canonicalize(&root).map_err(|error| {
        BrowserStagingStartupError::io(BrowserStagingStartupStage::Canonicalize, &error)
    })?;
    Ok((root, canonical_root))
}

fn validate_private_root_metadata(
    metadata: &fs::Metadata,
) -> Result<(), BrowserStagingStartupError> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(BrowserStagingStartupError::invalid_root(
            BrowserStagingStartupStage::Metadata,
        ));
    }
    Ok(())
}

fn maybe_inject_startup_failure(
    injected_failure: Option<(BrowserStagingStartupStage, std::io::ErrorKind)>,
    stage: BrowserStagingStartupStage,
) -> Result<(), BrowserStagingStartupError> {
    if let Some((failed_stage, kind)) = injected_failure
        && failed_stage == stage
    {
        return Err(BrowserStagingStartupError {
            stage,
            failure: BrowserStagingStartupFailure::Io,
            io_kind: Some(kind),
        });
    }
    Ok(())
}

fn unique_handle(prefix: &str, exists: impl Fn(&str) -> bool) -> Result<String, PrivateRootError> {
    for _ in 0..8 {
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let mut handle = String::from(prefix);
        for byte in random {
            use std::fmt::Write as _;
            let _ = write!(&mut handle, "{byte:02x}");
        }
        if !exists(&handle) {
            return Ok(handle);
        }
    }
    Err(PrivateRootError::Io(std::io::ErrorKind::Other))
}

fn safe_single_component(value: &str) -> bool {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return false;
    }
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn prove_direct_regular_file(
    path: &Path,
    expected_parent_or_root: &Path,
    expected_bytes: u64,
    max_bytes: u64,
) -> Result<PathBuf, PrivateRootError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PrivateRootError::InvalidRoot)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != expected_bytes
        || metadata.len() > max_bytes
    {
        return Err(PrivateRootError::InvalidRoot);
    }
    let canonical = fs::canonicalize(path).map_err(|_| PrivateRootError::InvalidRoot)?;
    if !(canonical.parent() == Some(expected_parent_or_root)
        || canonical.starts_with(expected_parent_or_root))
    {
        return Err(PrivateRootError::InvalidRoot);
    }
    Ok(canonical)
}

fn read_bounded_exact(
    path: &Path,
    max_bytes: u64,
    expected_bytes: u64,
) -> Result<Vec<u8>, BrowserDownloadStagingError> {
    let file = fs::File::open(path).map_err(|_| BrowserDownloadStagingError::Io)?;
    let mut data = Vec::with_capacity(usize::try_from(expected_bytes).unwrap_or(0));
    file.take(max_bytes + 1)
        .read_to_end(&mut data)
        .map_err(|_| BrowserDownloadStagingError::Io)?;
    if data.len() as u64 != expected_bytes || data.len() as u64 > max_bytes {
        return Err(BrowserDownloadStagingError::InvalidCompletedFile);
    }
    Ok(data)
}

fn remove_private_root(root: &Path) -> Result<(), PrivateRootError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(root).map_err(|error| PrivateRootError::Io(error.kind()))
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(root).map_err(|error| PrivateRootError::Io(error.kind()))
        }
        Ok(_) => Err(PrivateRootError::InvalidRoot),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PrivateRootError::Io(error.kind())),
    }
}

#[cfg(unix)]
fn harden_directory_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn harden_directory_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn harden_file_open_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn harden_file_open_options(_options: &mut OpenOptions) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const CONTEXT: &str = "ctx_0123456789abcdef0123456789abcdef";

    fn root() -> PathBuf {
        let mut random = [0_u8; 8];
        OsRng.fill_bytes(&mut random);
        let suffix = u64::from_le_bytes(random);
        std::env::temp_dir().join(format!(
            "cumg-transfer-stage-{}-{}-{suffix:016x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn startup_failure_stage_and_io_class_are_bounded_for_every_private_root_step() {
        let stages = [
            BrowserStagingStartupStage::RemoveExistingRoot,
            BrowserStagingStartupStage::CreateRoot,
            BrowserStagingStartupStage::SetPermissions,
            BrowserStagingStartupStage::Metadata,
            BrowserStagingStartupStage::Canonicalize,
        ];
        for stage in stages {
            let state = root();
            fs::create_dir_all(&state).unwrap();
            let error = create_private_root_inner(
                &state,
                "browser-upload-staging",
                Some((stage, std::io::ErrorKind::StorageFull)),
            )
            .unwrap_err();
            assert_eq!(error.stage(), stage.as_str());
            assert_eq!(error.failure_class(), "io");
            assert_eq!(error.io_class(), "storage_full");
            assert!(error.is_io());
            let rendered = format!("{error:?} {error}");
            assert!(!rendered.contains(state.to_string_lossy().as_ref()));
            let _ = fs::remove_dir_all(state);
        }
    }

    #[cfg(unix)]
    #[test]
    fn healthy_startup_roots_are_private_and_invalid_metadata_fails_closed() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let state = root();
        fs::create_dir_all(&state).unwrap();
        let upload = BrowserUploadStagingBroker::new(&state).unwrap();
        let download = BrowserDownloadStagingBroker::new(&state).unwrap();
        let upload_root = upload.inner.lock().unwrap().root.clone();
        let download_root = download.inner.lock().unwrap().root.clone();
        assert_eq!(
            fs::metadata(&upload_root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&download_root).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let regular_file = state.join("not-a-root");
        fs::write(&regular_file, b"not a directory").unwrap();
        let file_error =
            validate_private_root_metadata(&fs::symlink_metadata(&regular_file).unwrap())
                .unwrap_err();
        assert_eq!(file_error.stage(), "metadata");
        assert_eq!(file_error.failure_class(), "invalid_root");
        assert_eq!(file_error.io_class(), "not_applicable");

        let symlink_path = state.join("symlink-root");
        symlink(&regular_file, &symlink_path).unwrap();
        let symlink_error =
            validate_private_root_metadata(&fs::symlink_metadata(&symlink_path).unwrap())
                .unwrap_err();
        assert_eq!(symlink_error.stage(), "metadata");
        assert_eq!(symlink_error.failure_class(), "invalid_root");
        assert!(!symlink_error.is_io());
        drop(upload);
        drop(download);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn public_browser_staging_codes_remain_bounded() {
        assert_eq!(
            BrowserUploadStagingError::Io.safe_error_code(),
            "browser_upload_staging_io"
        );
        assert_eq!(
            BrowserUploadStagingError::InvalidRoot.safe_error_code(),
            "browser_upload_staging_invalid_root"
        );
        assert_eq!(
            BrowserDownloadStagingError::Io.safe_error_code(),
            "browser_download_staging_io"
        );
        assert_eq!(
            BrowserDownloadStagingError::InvalidRoot.safe_error_code(),
            "browser_download_staging_invalid_root"
        );
        let startup_io = BrowserStagingStartupError {
            stage: BrowserStagingStartupStage::CreateRoot,
            failure: BrowserStagingStartupFailure::Io,
            io_kind: Some(std::io::ErrorKind::StorageFull),
        };
        assert_eq!(
            startup_io.upload_safe_error_code(),
            "browser_upload_staging_io"
        );
        assert_eq!(
            startup_io.download_safe_error_code(),
            "browser_download_staging_io"
        );
        let startup_invalid =
            BrowserStagingStartupError::invalid_root(BrowserStagingStartupStage::Metadata);
        assert_eq!(
            startup_invalid.upload_safe_error_code(),
            "browser_upload_staging_invalid_root"
        );
        assert_eq!(
            startup_invalid.download_safe_error_code(),
            "browser_download_staging_invalid_root"
        );
    }

    #[test]
    fn upload_stages_resolves_and_consumes_private_regular_files() {
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserUploadStagingBroker::new(&state).unwrap();
        let staged = broker
            .stage(CONTEXT, 3, 7, "report.txt", &STANDARD.encode(b"hello"), 5)
            .unwrap();
        let resolved = broker.resolve(&staged.handle, CONTEXT, 3, 7).unwrap();
        assert!(Path::new(resolved.canonical_path()).is_file());
        assert!(!format!("{resolved:?}").contains("report.txt"));
        broker
            .consume_handles(std::slice::from_ref(&staged.handle), CONTEXT, 3, 7)
            .unwrap();
        assert_eq!(broker.len(), 0);
        assert_eq!(
            broker.resolve(&staged.handle, CONTEXT, 3, 7),
            Err(BrowserUploadStagingError::UnknownHandle)
        );
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn upload_rejects_cross_context_generation_revision_and_replacement() {
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserUploadStagingBroker::new(&state).unwrap();
        let staged = broker
            .stage(CONTEXT, 3, 7, "payload.bin", &STANDARD.encode(b"abc"), 3)
            .unwrap();
        assert_eq!(
            broker.resolve(&staged.handle, "ctx_ffffffffffffffffffffffffffffffff", 3, 7),
            Err(BrowserUploadStagingError::ContextMismatch)
        );
        assert_eq!(
            broker.resolve(&staged.handle, CONTEXT, 4, 7),
            Err(BrowserUploadStagingError::GenerationMismatch)
        );
        assert_eq!(
            broker.resolve(&staged.handle, CONTEXT, 3, 8),
            Err(BrowserUploadStagingError::CapabilityRevisionMismatch)
        );
        let path = broker
            .resolve(&staged.handle, CONTEXT, 3, 7)
            .unwrap()
            .canonical_path
            .clone();
        fs::write(&path, b"replacement").unwrap();
        assert_eq!(
            broker.resolve(&staged.handle, CONTEXT, 3, 7),
            Err(BrowserUploadStagingError::InvalidStagedFile)
        );
        let _ = fs::remove_dir_all(state);
    }

    #[cfg(unix)]
    #[test]
    fn upload_rejects_symlink_replacement_and_directory() {
        use std::os::unix::fs::symlink;
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserUploadStagingBroker::new(&state).unwrap();
        let staged = broker
            .stage(CONTEXT, 3, 7, "payload.bin", &STANDARD.encode(b"abc"), 3)
            .unwrap();
        let path = PathBuf::from(
            broker
                .resolve(&staged.handle, CONTEXT, 3, 7)
                .unwrap()
                .canonical_path,
        );
        fs::remove_file(&path).unwrap();
        symlink("/etc/hosts", &path).unwrap();
        assert_eq!(
            broker.resolve(&staged.handle, CONTEXT, 3, 7),
            Err(BrowserUploadStagingError::InvalidStagedFile)
        );
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(
            broker.resolve(&staged.handle, CONTEXT, 3, 7),
            Err(BrowserUploadStagingError::InvalidStagedFile)
        );
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn upload_rejects_missing_and_oversized_files() {
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserUploadStagingBroker::new(&state).unwrap();
        let staged = broker
            .stage(CONTEXT, 3, 7, "payload.bin", &STANDARD.encode(b"abc"), 3)
            .unwrap();
        let path = PathBuf::from(
            broker
                .resolve(&staged.handle, CONTEXT, 3, 7)
                .unwrap()
                .canonical_path,
        );
        fs::remove_file(path).unwrap();
        assert_eq!(
            broker.resolve(&staged.handle, CONTEXT, 3, 7),
            Err(BrowserUploadStagingError::InvalidStagedFile)
        );
        assert_eq!(
            broker.stage(
                CONTEXT,
                3,
                7,
                "too-large.bin",
                "",
                MAX_BROWSER_UPLOAD_FILE_BYTES as u64 + 1,
            ),
            Err(BrowserUploadStagingError::FileTooLarge)
        );
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn repeated_upload_context_lifecycle_plateaus_at_zero() {
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserUploadStagingBroker::new(&state).unwrap();
        for index in 0..100 {
            let context = format!("ctx_{index:032x}");
            broker
                .stage(
                    &context,
                    4,
                    9,
                    "payload.bin",
                    &STANDARD.encode([1_u8, 2, 3]),
                    3,
                )
                .unwrap();
            broker.cleanup_context(&context).unwrap();
            assert_eq!(broker.len(), 0);
        }
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn download_happy_path_is_private_bounded_and_opaque() {
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserDownloadStagingBroker::new(&state).unwrap();
        let prepared = broker
            .prepare(CONTEXT, 2, 5, "report.txt", 1024, false)
            .unwrap();
        let private_file = Path::new(prepared.canonical_root()).join("backend-guid");
        fs::write(&private_file, b"hello").unwrap();
        let result = broker
            .finalize(
                prepared.operation_handle(),
                CONTEXT,
                2,
                5,
                "backend-guid",
                5,
            )
            .unwrap();
        assert_eq!(result.bytes, 5);
        assert_eq!(STANDARD.decode(&result.data_base64).unwrap(), b"hello");
        assert!(result.backend_download_handle.starts_with("download_"));
        assert!(!format!("{result:?}").contains("backend-guid"));
        assert_eq!(broker.pending_len(), 0);
        assert_eq!(broker.completed_len(), 1);
        broker.cleanup_context(CONTEXT).unwrap();
        assert_eq!(broker.completed_len(), 0);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn download_rejects_collision_and_honors_overwrite_after_success() {
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserDownloadStagingBroker::new(&state).unwrap();
        let first = broker
            .prepare(CONTEXT, 2, 5, "report.txt", 1024, false)
            .unwrap();
        fs::write(Path::new(first.canonical_root()).join("one"), b"one").unwrap();
        broker
            .finalize(first.operation_handle(), CONTEXT, 2, 5, "one", 3)
            .unwrap();
        assert_eq!(
            broker.prepare(CONTEXT, 2, 5, "report.txt", 1024, false),
            Err(BrowserDownloadStagingError::Collision)
        );
        let replacement = broker
            .prepare(CONTEXT, 2, 5, "report.txt", 1024, true)
            .unwrap();
        fs::write(Path::new(replacement.canonical_root()).join("two"), b"two").unwrap();
        broker
            .finalize(replacement.operation_handle(), CONTEXT, 2, 5, "two", 3)
            .unwrap();
        assert_eq!(broker.completed_len(), 1);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn download_rejects_cross_session_oversize_and_unsafe_backend_id() {
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserDownloadStagingBroker::new(&state).unwrap();
        let prepared = broker
            .prepare(CONTEXT, 2, 5, "report.txt", 4, false)
            .unwrap();
        assert_eq!(
            broker.finalize(
                prepared.operation_handle(),
                "ctx_ffffffffffffffffffffffffffffffff",
                2,
                5,
                "guid",
                1,
            ),
            Err(BrowserDownloadStagingError::ContextMismatch)
        );
        fs::write(Path::new(prepared.canonical_root()).join("guid"), b"12345").unwrap();
        assert_eq!(
            broker.finalize(prepared.operation_handle(), CONTEXT, 2, 5, "guid", 5),
            Err(BrowserDownloadStagingError::FileTooLarge)
        );
        let prepared = broker
            .prepare(CONTEXT, 2, 5, "second.txt", 16, false)
            .unwrap();
        assert_eq!(
            broker.finalize(prepared.operation_handle(), CONTEXT, 2, 5, "../escape", 1),
            Err(BrowserDownloadStagingError::InvalidBackendId)
        );
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn download_rejects_partial_completion_and_discards_pending_result() {
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserDownloadStagingBroker::new(&state).unwrap();
        let prepared = broker
            .prepare(CONTEXT, 2, 5, "partial.bin", 1024, false)
            .unwrap();
        fs::write(Path::new(prepared.canonical_root()).join("guid"), b"abc").unwrap();
        assert_eq!(
            broker.finalize(prepared.operation_handle(), CONTEXT, 2, 5, "guid", 5),
            Err(BrowserDownloadStagingError::InvalidCompletedFile)
        );
        assert_eq!(broker.pending_len(), 0);
        assert_eq!(broker.completed_len(), 0);
        let _ = fs::remove_dir_all(state);
    }

    #[cfg(unix)]
    #[test]
    fn download_rejects_symlink_and_directory_completion() {
        use std::os::unix::fs::symlink;
        let state = root();
        fs::create_dir_all(&state).unwrap();
        let broker = BrowserDownloadStagingBroker::new(&state).unwrap();
        let prepared = broker
            .prepare(CONTEXT, 2, 5, "report.txt", 1024, false)
            .unwrap();
        symlink(
            "/etc/hosts",
            Path::new(prepared.canonical_root()).join("guid"),
        )
        .unwrap();
        assert_eq!(
            broker.finalize(prepared.operation_handle(), CONTEXT, 2, 5, "guid", 1),
            Err(BrowserDownloadStagingError::InvalidCompletedFile)
        );
        let prepared = broker
            .prepare(CONTEXT, 2, 5, "second.txt", 1024, false)
            .unwrap();
        fs::create_dir(Path::new(prepared.canonical_root()).join("guid")).unwrap();
        assert_eq!(
            broker.finalize(prepared.operation_handle(), CONTEXT, 2, 5, "guid", 0),
            Err(BrowserDownloadStagingError::InvalidCompletedFile)
        );
        let _ = fs::remove_dir_all(state);
    }
}
