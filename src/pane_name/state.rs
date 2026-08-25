use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    marker::PhantomData,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(test)]
use std::cell::Cell;

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, Visitor},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) const SCHEMA_VERSION: u32 = 1;

const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const TEMP_ATTEMPTS: u64 = 64;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedState {
    pub(crate) schema_version: u32,
    pub(crate) socket_digest: String,
    pub(crate) cleanup_pending: bool,
    #[serde(deserialize_with = "deserialize_unique_map")]
    pub(crate) panes: BTreeMap<String, PaneState>,
}

fn deserialize_unique_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct UniqueMapVisitor<V>(PhantomData<V>);

    impl<'de, V> Visitor<'de> for UniqueMapVisitor<V>
    where
        V: Deserialize<'de>,
    {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map with unique keys")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if values.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate map key"));
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(UniqueMapVisitor(PhantomData))
}

impl PersistedState {
    pub(crate) fn empty(socket_digest: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            socket_digest,
            cleanup_pending: false,
            panes: BTreeMap::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PaneState {
    pub(crate) terminal_digest: String,
    pub(crate) baseline: Option<String>,
    pub(crate) selection: Option<Selection>,
    #[serde(deserialize_with = "deserialize_unique_map")]
    pub(crate) overrides: BTreeMap<String, Option<String>>,
    pub(crate) applied: Applied,
    pub(crate) pending: Option<PendingTransition>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Selection {
    pub(crate) generation_digest: String,
    pub(crate) identity_digest: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Applied {
    pub(crate) target_digest: String,
    pub(crate) source: AppliedSource,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AppliedSource {
    Generated { identity_digest: String },
    Override { identity_digest: String },
    Baseline,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingTransition {
    pub(crate) prior_digest: String,
    pub(crate) target_digest: String,
    pub(crate) disposition: PendingDisposition,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PendingDisposition {
    Keep { source: AppliedSource },
    Release,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum StateError {
    #[error("state directory is unavailable")]
    Directory,
    #[error("state directory is unsafe")]
    UnsafeDirectory,
    #[error("state file is unavailable")]
    File,
    #[error("state file is unsafe")]
    UnsafeFile,
    #[error("state document is malformed")]
    Malformed,
    #[error("state document uses an unsupported schema version")]
    UnsupportedVersion,
    #[error("state document belongs to a different socket")]
    SocketMismatch,
    #[error("state file write failed")]
    Write,
    #[error("state file rename failed")]
    Rename,
    #[error("state file sync failed")]
    Sync,
}

pub(crate) struct StateFile {
    directory_path: PathBuf,
    directory: File,
    file_name: std::ffi::CString,
    #[cfg(test)]
    path: PathBuf,
    socket_digest: String,
    #[cfg(test)]
    fail_next_persist: Cell<Option<StateError>>,
}

impl StateFile {
    pub(crate) fn open(
        state_dir: &Path,
        socket_path: &Path,
    ) -> Result<(Self, PersistedState), StateError> {
        let directory_path = state_dir.join("pane-name");
        create_or_validate_directory(&directory_path)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&directory_path)
            .map_err(|_| StateError::Directory)?;
        validate_directory(&directory.metadata().map_err(|_| StateError::Directory)?)?;
        validate_directory_path(&directory_path, &directory)?;

        let socket_digest = digest_socket_path(socket_path);
        let file_name = c_string(&format!("{socket_digest}.json"))?;
        #[cfg(test)]
        let path = directory_path.join(file_name.to_string_lossy().as_ref());
        let file = Self {
            directory_path,
            directory,
            file_name,
            #[cfg(test)]
            path,
            socket_digest: socket_digest.clone(),
            #[cfg(test)]
            fail_next_persist: Cell::new(None),
        };

        let state = match file.open_at(&file.file_name, libc::O_RDONLY) {
            Ok(reader) => serde_json::from_reader(reader).map_err(|_| StateError::Malformed)?,
            Err(error) => {
                if !file.entry_exists(&file.file_name)? {
                    PersistedState::empty(socket_digest)
                } else {
                    file.validate_existing_entry()?;
                    return Err(error);
                }
            }
        };

        validate_state(&state, &file.socket_digest)?;
        Ok((file, state))
    }

    pub(crate) fn persist(&self, state: &PersistedState) -> Result<(), StateError> {
        #[cfg(test)]
        if let Some(error) = self.fail_next_persist.take() {
            return Err(error);
        }

        validate_state(state, &self.socket_digest)?;
        self.validate_directory_handle()?;
        self.validate_existing_entry()?;

        let (temporary_name, mut temporary_file) = self.create_temporary_file()?;
        let result = (|| {
            serde_json::to_writer(&mut temporary_file, state).map_err(|_| StateError::Write)?;
            temporary_file
                .write_all(b"\n")
                .map_err(|_| StateError::Write)?;
            temporary_file.flush().map_err(|_| StateError::Write)?;
            temporary_file.sync_all().map_err(|_| StateError::Sync)?;
            self.verify_entry_matches(&temporary_name, &temporary_file)?;

            let renamed = unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    temporary_name.as_ptr(),
                    self.directory.as_raw_fd(),
                    self.file_name.as_ptr(),
                )
            };
            if renamed != 0 {
                return Err(StateError::Rename);
            }
            self.verify_entry_matches(&self.file_name, &temporary_file)?;
            self.directory.sync_all().map_err(|_| StateError::Sync)?;
            self.validate_directory_handle()?;
            Ok(())
        })();

        if result.is_err() {
            let _ = self.unlink_at(&temporary_name);
        }
        result
    }

    pub(crate) fn remove(&self) -> Result<(), StateError> {
        self.validate_directory_handle()?;
        if !self.entry_exists(&self.file_name)? {
            return Ok(());
        }
        self.validate_existing_entry()?;
        self.unlink_at(&self.file_name)?;
        self.directory.sync_all().map_err(|_| StateError::Sync)?;
        self.validate_directory_handle()
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn fail_next_persist(&self, error: StateError) {
        self.fail_next_persist.set(Some(error));
    }

    fn create_temporary_file(&self) -> Result<(std::ffi::CString, File), StateError> {
        for _ in 0..TEMP_ATTEMPTS {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = c_string(&format!(
                ".{}.{}.{}",
                self.socket_digest,
                std::process::id(),
                sequence
            ))?;
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    FILE_MODE,
                )
            };
            if fd >= 0 {
                let file = unsafe { File::from_raw_fd(fd) };
                if unsafe { libc::fchmod(file.as_raw_fd(), FILE_MODE as libc::mode_t) } != 0 {
                    drop(file);
                    let _ = self.unlink_at(&name);
                    return Err(StateError::Write);
                }
                validate_regular_file(&file.metadata().map_err(|_| StateError::File)?)?;
                return Ok((name, file));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(StateError::Write);
            }
        }
        Err(StateError::Write)
    }

    fn open_at(&self, name: &std::ffi::CStr, flags: i32) -> Result<File, StateError> {
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(StateError::File);
        }
        let file = unsafe { File::from_raw_fd(fd) };
        validate_regular_file(&file.metadata().map_err(|_| StateError::File)?)?;
        Ok(file)
    }

    fn entry_exists(&self, name: &std::ffi::CStr) -> Result<bool, StateError> {
        match entry_stat(self.directory.as_raw_fd(), name) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(StateError::File),
        }
    }

    fn validate_existing_entry(&self) -> Result<(), StateError> {
        match entry_stat(self.directory.as_raw_fd(), &self.file_name) {
            Ok(stat) => validate_stat(&stat),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(StateError::File),
        }
    }

    fn verify_entry_matches(&self, name: &std::ffi::CStr, file: &File) -> Result<(), StateError> {
        let stat = entry_stat(self.directory.as_raw_fd(), name).map_err(|_| StateError::Rename)?;
        validate_stat(&stat)?;
        let metadata = file.metadata().map_err(|_| StateError::File)?;
        if stat.st_dev as u64 != metadata.dev() || stat.st_ino as u64 != metadata.ino() {
            return Err(StateError::Rename);
        }
        Ok(())
    }

    fn unlink_at(&self, name: &std::ffi::CStr) -> Result<(), StateError> {
        let result = unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) };
        if result != 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
            return Err(StateError::File);
        }
        Ok(())
    }

    fn validate_directory_handle(&self) -> Result<(), StateError> {
        validate_directory(
            &self
                .directory
                .metadata()
                .map_err(|_| StateError::Directory)?,
        )?;
        validate_directory_path(&self.directory_path, &self.directory)
    }
}

pub(crate) fn digest_pane_id(pane_id: &str) -> String {
    digest_parts(
        b"herdr-agent-context/pane-name/pane/v1",
        &[pane_id.as_bytes()],
    )
}

pub(crate) fn digest_label(value: Option<&str>) -> String {
    match value {
        Some(value) => digest_parts(
            b"herdr-agent-context/pane-name/label/v1",
            &[b"value", value.as_bytes()],
        ),
        None => digest_parts(b"herdr-agent-context/pane-name/label/v1", &[b"null"]),
    }
}

pub(crate) fn digest_identity(terminal_id: &str, agent: &str, identity: &str) -> String {
    digest_parts(
        b"herdr-agent-context/pane-name/identity/v1",
        &[
            terminal_id.as_bytes(),
            agent.as_bytes(),
            identity.as_bytes(),
        ],
    )
}

pub(crate) fn digest_terminal(terminal_id: &str) -> String {
    digest_parts(
        b"herdr-agent-context/pane-name/terminal/v1",
        &[terminal_id.as_bytes()],
    )
}

pub(crate) fn digest_generation(terminal_id: &str, agent: &str, binding_identity: &[u8]) -> String {
    digest_parts(
        b"herdr-agent-context/pane-name/generation/v1",
        &[terminal_id.as_bytes(), agent.as_bytes(), binding_identity],
    )
}

fn digest_socket_path(socket_path: &Path) -> String {
    digest_parts(
        b"herdr-agent-context/pane-name/socket/v1",
        &[socket_path.as_os_str().as_bytes()],
    )
}

fn digest_parts(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    frame(&mut hasher, domain);
    hasher.update((parts.len() as u64).to_be_bytes());
    for part in parts {
        frame(&mut hasher, part);
    }
    format!("{:x}", hasher.finalize())
}

fn frame(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn create_or_validate_directory(path: &Path) -> Result<(), StateError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(DIRECTORY_MODE);
            match builder.create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(StateError::Directory),
            }
            let metadata = fs::symlink_metadata(path).map_err(|_| StateError::Directory)?;
            validate_directory(&metadata)
        }
        Err(_) => Err(StateError::Directory),
    }
}

fn validate_directory(metadata: &fs::Metadata) -> Result<(), StateError> {
    let mode = metadata.mode();
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || mode & 0o077 != 0
    {
        return Err(StateError::UnsafeDirectory);
    }
    Ok(())
}

fn validate_directory_path(path: &Path, directory: &File) -> Result<(), StateError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|_| StateError::Directory)?;
    validate_directory(&path_metadata)?;
    let handle_metadata = directory.metadata().map_err(|_| StateError::Directory)?;
    if path_metadata.dev() != handle_metadata.dev() || path_metadata.ino() != handle_metadata.ino()
    {
        return Err(StateError::UnsafeDirectory);
    }
    Ok(())
}

fn validate_regular_file(metadata: &fs::Metadata) -> Result<(), StateError> {
    let mode = metadata.mode();
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || mode & 0o077 != 0
    {
        return Err(StateError::UnsafeFile);
    }
    Ok(())
}

fn c_string(value: &str) -> Result<std::ffi::CString, StateError> {
    std::ffi::CString::new(value).map_err(|_| StateError::File)
}

fn entry_stat(directory_fd: i32, name: &std::ffi::CStr) -> std::io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    let result = unsafe {
        libc::fstatat(
            directory_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(unsafe { stat.assume_init() })
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn validate_stat(stat: &libc::stat) -> Result<(), StateError> {
    let mode = stat.st_mode as libc::mode_t;
    if mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_uid != unsafe { libc::geteuid() }
        || mode & 0o077 != 0
    {
        return Err(StateError::UnsafeFile);
    }
    Ok(())
}

fn validate_state(state: &PersistedState, expected_socket_digest: &str) -> Result<(), StateError> {
    if state.schema_version != SCHEMA_VERSION {
        return Err(StateError::UnsupportedVersion);
    }
    validate_digest(&state.socket_digest)?;
    if state.socket_digest != expected_socket_digest {
        return Err(StateError::SocketMismatch);
    }
    validate_panes(&state.panes)?;
    Ok(())
}

fn validate_panes(panes: &BTreeMap<String, PaneState>) -> Result<(), StateError> {
    for (pane_digest, pane) in panes {
        validate_digest(pane_digest)?;
        validate_pane(pane)?;
    }
    Ok(())
}

fn validate_pane(pane: &PaneState) -> Result<(), StateError> {
    validate_digest(&pane.terminal_digest)?;
    if let Some(selection) = &pane.selection {
        validate_digest(&selection.generation_digest)?;
        validate_digest(&selection.identity_digest)?;
    }
    for identity_digest in pane.overrides.keys() {
        validate_digest(identity_digest)?;
    }
    validate_digest(&pane.applied.target_digest)?;
    validate_applied_source(&pane.applied.source)?;
    validate_source_target(&pane.applied.source, &pane.applied.target_digest, pane)?;
    if pane.pending.is_none() {
        validate_source_reachability(&pane.applied.source, pane)?;
    }
    if let Some(pending) = &pane.pending {
        validate_digest(&pending.prior_digest)?;
        validate_digest(&pending.target_digest)?;
        match &pending.disposition {
            PendingDisposition::Keep { source } => {
                validate_applied_source(source)?;
                validate_source_reachability(source, pane)?;
                validate_source_target(source, &pending.target_digest, pane)?;
            }
            PendingDisposition::Release => validate_baseline_target(&pending.target_digest, pane)?,
        }
    }
    Ok(())
}

fn validate_source_target(
    source: &AppliedSource,
    target_digest: &str,
    pane: &PaneState,
) -> Result<(), StateError> {
    match source {
        AppliedSource::Override { identity_digest } => {
            let target = pane
                .overrides
                .get(identity_digest)
                .ok_or(StateError::Malformed)?;
            if digest_label(target.as_deref()) != target_digest {
                return Err(StateError::Malformed);
            }
        }
        AppliedSource::Baseline => validate_baseline_target(target_digest, pane)?,
        AppliedSource::Generated { .. } => {}
    }
    Ok(())
}

fn validate_baseline_target(target_digest: &str, pane: &PaneState) -> Result<(), StateError> {
    if digest_label(pane.baseline.as_deref()) != target_digest {
        return Err(StateError::Malformed);
    }
    Ok(())
}

fn validate_source_reachability(
    source: &AppliedSource,
    pane: &PaneState,
) -> Result<(), StateError> {
    let identity_digest = match source {
        AppliedSource::Generated { identity_digest }
        | AppliedSource::Override { identity_digest } => identity_digest,
        AppliedSource::Baseline => return Ok(()),
    };
    if pane
        .selection
        .as_ref()
        .map(|selection| &selection.identity_digest)
        != Some(identity_digest)
    {
        return Err(StateError::Malformed);
    }
    if matches!(source, AppliedSource::Override { .. })
        && !pane.overrides.contains_key(identity_digest)
    {
        return Err(StateError::Malformed);
    }
    Ok(())
}

fn validate_applied_source(source: &AppliedSource) -> Result<(), StateError> {
    match source {
        AppliedSource::Generated { identity_digest }
        | AppliedSource::Override { identity_digest } => validate_digest(identity_digest),
        AppliedSource::Baseline => Ok(()),
    }
}

fn validate_digest(value: &str) -> Result<(), StateError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(StateError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        path::Path,
        time::Instant,
    };

    use tempfile::tempdir;

    use super::*;
    use crate::pane_name::PaneNameManager;
    use crate::tab_name::{
        DisplayState as TabDisplayState, PaneContext as TabPaneContext,
        PaneSnapshot as TabPaneSnapshot, TabNameManager, TabSnapshot,
    };

    const SOCKET_A: &str = "/synthetic/herdr/pane-socket-a";
    const SOCKET_B: &str = "/synthetic/herdr/pane-socket-b";
    const GENERATED_TITLE: &str = "synthetic generated pane title must not persist";
    const RAW_IDENTITY: &str = "synthetic-pane-session-identity";
    const RAW_PANE_ID: &str = "synthetic-pane-id";
    const RAW_TERMINAL_ID: &str = "synthetic-terminal-id";
    const RAW_BINDING_IDENTITY: &str = "/synthetic/private/binding.jsonl";

    fn sample_pane_state() -> PaneState {
        let identity_digest = digest_identity(RAW_TERMINAL_ID, "pi", RAW_IDENTITY);
        PaneState {
            terminal_digest: digest_terminal(RAW_TERMINAL_ID),
            baseline: Some("Manual pane baseline".to_owned()),
            selection: Some(Selection {
                generation_digest: digest_generation(
                    RAW_TERMINAL_ID,
                    "pi",
                    RAW_BINDING_IDENTITY.as_bytes(),
                ),
                identity_digest: identity_digest.clone(),
            }),
            overrides: BTreeMap::from([(
                identity_digest.clone(),
                Some("Manual pane override".to_owned()),
            )]),
            applied: Applied {
                target_digest: digest_label(Some(GENERATED_TITLE)),
                source: AppliedSource::Generated {
                    identity_digest: identity_digest.clone(),
                },
            },
            pending: Some(PendingTransition {
                prior_digest: digest_label(Some("previous generated pane title")),
                target_digest: digest_label(Some(GENERATED_TITLE)),
                disposition: PendingDisposition::Keep {
                    source: AppliedSource::Generated { identity_digest },
                },
            }),
        }
    }

    fn sample_state(socket_digest: String) -> PersistedState {
        PersistedState {
            schema_version: SCHEMA_VERSION,
            socket_digest,
            cleanup_pending: true,
            panes: BTreeMap::from([(digest_pane_id(RAW_PANE_ID), sample_pane_state())]),
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn write_owner_only(path: &Path, contents: &[u8]) {
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE)).unwrap();
    }

    fn only_file(directory: &Path) -> PathBuf {
        let entries: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(entries.len(), 1);
        entries.into_iter().next().unwrap()
    }

    #[test]
    fn nullable_label_digest_separates_null_empty_and_nonempty_values() {
        let null = digest_label(None);
        let empty = digest_label(Some(""));
        let value = digest_label(Some("manual"));

        assert_ne!(null, empty);
        assert_ne!(null, value);
        assert_ne!(empty, value);
        assert_eq!(null.len(), 64);
    }

    #[test]
    fn creates_owner_only_independent_directory_and_file() {
        let temporary = tempdir().unwrap();
        let (file, state) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();

        assert_eq!(mode(&temporary.path().join("pane-name")), DIRECTORY_MODE);
        file.persist(&state).unwrap();
        assert_eq!(mode(file.path()), FILE_MODE);
        assert_eq!(fs::metadata(file.path()).unwrap().uid(), unsafe {
            libc::geteuid()
        });
        assert!(!temporary.path().join("tab-name").exists());
    }

    #[test]
    fn scopes_state_by_socket_without_storing_the_raw_path() {
        let temporary = tempdir().unwrap();
        let (first, first_state) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        let (second, second_state) =
            StateFile::open(temporary.path(), Path::new(SOCKET_B)).unwrap();

        assert_ne!(first.path(), second.path());
        assert_ne!(first_state.socket_digest, second_state.socket_digest);
        assert!(!first.path().to_string_lossy().contains("pane-socket-a"));
        assert!(!second.path().to_string_lossy().contains("pane-socket-b"));
    }

    #[test]
    fn atomically_replaces_and_loads_nullable_state_without_private_plaintext() {
        let temporary = tempdir().unwrap();
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        let state = sample_state(initial.socket_digest);

        file.persist(&state).unwrap();
        file.persist(&state).unwrap();
        let (_, loaded) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        assert!(loaded == state);
        let serialized = fs::read_to_string(file.path()).unwrap();
        assert!(serialized.contains("Manual pane baseline"));
        assert!(serialized.contains("Manual pane override"));
        assert!(!serialized.contains(GENERATED_TITLE));
        assert!(!serialized.contains(RAW_IDENTITY));
        assert!(!serialized.contains(RAW_PANE_ID));
        assert!(!serialized.contains(RAW_TERMINAL_ID));
        assert!(!serialized.contains(RAW_BINDING_IDENTITY));
        assert!(!serialized.contains(SOCKET_A));
        assert!(
            fs::read_dir(temporary.path().join("pane-name"))
                .unwrap()
                .all(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".json"))
        );
    }

    #[test]
    fn round_trips_null_baseline_and_manual_clear_override() {
        let temporary = tempdir().unwrap();
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        let mut state = sample_state(initial.socket_digest);
        let pane = state.panes.values_mut().next().unwrap();
        let identity_digest = pane.selection.as_ref().unwrap().identity_digest.clone();
        pane.baseline = None;
        pane.overrides.insert(identity_digest.clone(), None);
        pane.applied = Applied {
            target_digest: digest_label(None),
            source: AppliedSource::Override { identity_digest },
        };
        pane.pending = None;

        file.persist(&state).unwrap();
        let (_, loaded) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        let loaded = loaded.panes.values().next().unwrap();
        assert_eq!(loaded.baseline, None);
        assert!(loaded.overrides.values().any(Option::is_none));
    }

    #[test]
    fn rejects_malformed_future_and_wrong_socket_documents_without_rewriting_them() {
        let temporary = tempdir().unwrap();
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();

        write_owner_only(file.path(), b"{");
        let malformed = fs::read(file.path()).unwrap();
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::Malformed)
        ));
        assert_eq!(fs::read(file.path()).unwrap(), malformed);

        let mut future = PersistedState::empty(initial.socket_digest.clone());
        future.schema_version += 1;
        let future = serde_json::to_vec(&future).unwrap();
        write_owner_only(file.path(), &future);
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsupportedVersion)
        ));
        assert_eq!(fs::read(file.path()).unwrap(), future);

        let mut mismatched = sample_state(initial.socket_digest);
        mismatched.socket_digest = digest_label(Some("another socket"));
        let mismatched = serde_json::to_vec(&mismatched).unwrap();
        write_owner_only(file.path(), &mismatched);
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::SocketMismatch)
        ));
        assert_eq!(fs::read(file.path()).unwrap(), mismatched);
    }

    #[test]
    fn rejects_duplicate_fields_in_final_schema() {
        let temporary = tempdir().unwrap();
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        let state = sample_state(initial.socket_digest);
        let serialized = serde_json::to_string(&state).unwrap();
        let duplicate_field = serialized.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
            1,
        );
        let pane_key = digest_pane_id(RAW_PANE_ID);
        let pane_entry = format!(
            "{}:{}",
            serde_json::to_string(&pane_key).unwrap(),
            serde_json::to_string(&sample_pane_state()).unwrap()
        );
        let duplicate_pane =
            serialized.replacen(&pane_entry, &format!("{pane_entry},{pane_entry}"), 1);
        let pane = sample_pane_state();
        let override_key = pane.overrides.keys().next().unwrap();
        let override_entry = format!(
            "{}:{}",
            serde_json::to_string(override_key).unwrap(),
            serde_json::to_string(pane.overrides.get(override_key).unwrap()).unwrap()
        );
        let duplicate_override = serialized.replacen(
            &override_entry,
            &format!("{override_entry},{override_entry}"),
            1,
        );

        for duplicate in [duplicate_field, duplicate_pane, duplicate_override] {
            write_owner_only(file.path(), duplicate.as_bytes());
            assert!(matches!(
                StateFile::open(temporary.path(), Path::new(SOCKET_A)),
                Err(StateError::Malformed)
            ));
            assert_eq!(fs::read_to_string(file.path()).unwrap(), duplicate);
        }
    }

    #[test]
    fn rejects_unsafe_directory_and_file_permissions() {
        let temporary = tempdir().unwrap();
        let directory = temporary.path().join("pane-name");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsafeDirectory)
        ));

        let isolated = tempdir().unwrap();
        let (file, state) = StateFile::open(isolated.path(), Path::new(SOCKET_A)).unwrap();
        file.persist(&state).unwrap();
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            StateFile::open(isolated.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsafeFile)
        ));
    }

    #[test]
    fn rejects_symlinked_directory_and_state_file() {
        let temporary = tempdir().unwrap();
        let redirected = tempdir().unwrap();
        symlink(redirected.path(), temporary.path().join("pane-name")).unwrap();
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsafeDirectory)
        ));

        let isolated = tempdir().unwrap();
        let (file, _) = StateFile::open(isolated.path(), Path::new(SOCKET_A)).unwrap();
        let target = isolated.path().join("other-state");
        write_owner_only(&target, b"synthetic");
        symlink(&target, file.path()).unwrap();
        assert!(matches!(
            StateFile::open(isolated.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsafeFile)
        ));
    }

    #[test]
    fn detects_atomic_entry_and_directory_substitution() {
        let temporary = tempdir().unwrap();
        let (file, state) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        let (name, original) = file.create_temporary_file().unwrap();
        let temporary_path = temporary
            .path()
            .join("pane-name")
            .join(name.to_string_lossy().as_ref());
        fs::remove_file(&temporary_path).unwrap();
        write_owner_only(&temporary_path, b"replacement");
        assert!(matches!(
            file.verify_entry_matches(&name, &original),
            Err(StateError::Rename)
        ));
        fs::remove_file(&temporary_path).unwrap();

        let directory = temporary.path().join("pane-name");
        fs::rename(&directory, temporary.path().join("old-pane-name")).unwrap();
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(DIRECTORY_MODE)).unwrap();
        assert!(matches!(
            file.persist(&state),
            Err(StateError::UnsafeDirectory)
        ));
        assert!(fs::read_dir(&directory).unwrap().next().is_none());
    }

    #[test]
    fn write_sync_and_rename_failures_are_reported_without_replacing_state() {
        let temporary = tempdir().unwrap();
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        let state = sample_state(initial.socket_digest);

        for error in [StateError::Write, StateError::Sync, StateError::Rename] {
            file.fail_next_persist(error);
            assert_eq!(file.persist(&state), Err(error));
            assert!(!file.path().exists());
        }

        let directory = temporary.path().join("pane-name");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(file.persist(&state), Err(StateError::Write));
        fs::set_permissions(&directory, fs::Permissions::from_mode(DIRECTORY_MODE)).unwrap();
    }

    #[test]
    fn rejects_semantically_invalid_nullable_override_digest() {
        let temporary = tempdir().unwrap();
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).unwrap();
        let mut state = sample_state(initial.socket_digest);
        let pane = state.panes.values_mut().next().unwrap();
        let identity_digest = pane.selection.as_ref().unwrap().identity_digest.clone();
        pane.pending = None;
        pane.applied = Applied {
            target_digest: digest_label(None),
            source: AppliedSource::Override { identity_digest },
        };
        let invalid = serde_json::to_vec(&state).unwrap();
        write_owner_only(file.path(), &invalid);

        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::Malformed)
        ));
        assert_eq!(fs::read(file.path()).unwrap(), invalid);
    }

    #[test]
    fn pane_and_tab_state_failures_and_writes_are_independent() {
        let temporary = tempdir().unwrap();
        let socket = Path::new(SOCKET_A);
        let mut tab_manager = TabNameManager::load(temporary.path(), socket).unwrap();
        tab_manager
            .reconcile(
                true,
                &[TabSnapshot {
                    tab_id: "tab-a".into(),
                    workspace_id: "workspace-a".into(),
                    position: 1,
                    observed_label: "baseline".into(),
                }],
                &[TabPaneSnapshot {
                    pane_id: "tab-pane-a".into(),
                    terminal_id: "tab-terminal-a".into(),
                    binding_identity: Some(b"tab-binding-a".to_vec()),
                    tab_id: "tab-a".into(),
                    context: TabPaneContext::Supported {
                        agent: "pi".into(),
                        identity: "tab-session-a".into(),
                        display: TabDisplayState::Resolved("Tab generated".into()),
                    },
                }],
                Instant::now(),
            )
            .unwrap();
        drop(tab_manager);
        let tab_path = only_file(&temporary.path().join("tab-name"));
        let tab_before = fs::read(&tab_path).unwrap();

        let (pane_file, initial) = StateFile::open(temporary.path(), socket).unwrap();
        pane_file
            .persist(&sample_state(initial.socket_digest))
            .unwrap();
        assert_eq!(fs::read(&tab_path).unwrap(), tab_before);

        write_owner_only(pane_file.path(), b"{");
        assert!(TabNameManager::load(temporary.path(), socket).is_ok());

        write_owner_only(&tab_path, b"{");
        fs::remove_file(pane_file.path()).unwrap();
        assert!(PaneNameManager::load(temporary.path(), socket).is_ok());
    }
}
