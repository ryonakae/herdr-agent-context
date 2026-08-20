use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
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
    pub(crate) tabs: BTreeMap<String, TabState>,
}

impl PersistedState {
    pub(crate) fn empty(socket_digest: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            socket_digest,
            cleanup_pending: false,
            tabs: BTreeMap::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TabState {
    pub(crate) baseline: Baseline,
    pub(crate) selection: Option<Selection>,
    pub(crate) overrides: BTreeMap<String, String>,
    pub(crate) applied: Option<Applied>,
    pub(crate) pending: Option<PendingTransition>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Baseline {
    Exact { value: String },
    ProbableAuto,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Selection {
    pub(crate) pane_id: String,
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

#[derive(Debug, Error, PartialEq, Eq)]
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
}

impl StateFile {
    pub(crate) fn open(
        state_dir: &Path,
        socket_path: &Path,
    ) -> Result<(Self, PersistedState), StateError> {
        let directory_path = state_dir.join("tab-name");
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

pub(crate) fn digest_label(value: &str) -> String {
    digest_parts(
        b"herdr-agent-context/tab-name/label/v1",
        &[value.as_bytes()],
    )
}

pub(crate) fn digest_identity(tab_id: &str, agent: &str, identity: &str) -> String {
    digest_parts(
        b"herdr-agent-context/tab-name/identity/v1",
        &[tab_id.as_bytes(), agent.as_bytes(), identity.as_bytes()],
    )
}

fn digest_socket_path(socket_path: &Path) -> String {
    digest_parts(
        b"herdr-agent-context/tab-name/socket/v1",
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
    match state.schema_version.cmp(&SCHEMA_VERSION) {
        std::cmp::Ordering::Greater => return Err(StateError::UnsupportedVersion),
        std::cmp::Ordering::Less => return Err(StateError::UnsupportedVersion),
        std::cmp::Ordering::Equal => {}
    }
    if !is_digest(&state.socket_digest) {
        return Err(StateError::Malformed);
    }
    if state.socket_digest != expected_socket_digest {
        return Err(StateError::SocketMismatch);
    }
    for tab in state.tabs.values() {
        validate_tab(tab)?;
    }
    Ok(())
}

fn validate_tab(tab: &TabState) -> Result<(), StateError> {
    if let Some(selection) = &tab.selection {
        validate_digest(&selection.identity_digest)?;
    }
    for identity_digest in tab.overrides.keys() {
        validate_digest(identity_digest)?;
    }
    if let Some(applied) = &tab.applied {
        validate_digest(&applied.target_digest)?;
        validate_applied_source(&applied.source)?;
    }
    if let Some(pending) = &tab.pending {
        validate_digest(&pending.prior_digest)?;
        validate_digest(&pending.target_digest)?;
        if let PendingDisposition::Keep { source } = &pending.disposition {
            validate_applied_source(source)?;
        }
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
    if is_digest(value) {
        Ok(())
    } else {
        Err(StateError::Malformed)
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        path::Path,
    };

    use tempfile::tempdir;

    use super::*;

    const SOCKET_A: &str = "/synthetic/herdr/socket-a";
    const SOCKET_B: &str = "/synthetic/herdr/socket-b";
    const GENERATED_TITLE: &str = "synthetic generated title must not persist";
    const RAW_IDENTITY: &str = "synthetic-identity-not-for-storage";

    fn sample_tab_state() -> TabState {
        let identity_digest = digest_identity("tab-a", "pi", RAW_IDENTITY);
        TabState {
            baseline: Baseline::Exact {
                value: "Manual baseline label".to_owned(),
            },
            selection: Some(Selection {
                pane_id: "pane-a".to_owned(),
                identity_digest: identity_digest.clone(),
            }),
            overrides: BTreeMap::from([(
                identity_digest.clone(),
                "Manual override label".to_owned(),
            )]),
            applied: Some(Applied {
                target_digest: digest_label(GENERATED_TITLE),
                source: AppliedSource::Generated {
                    identity_digest: identity_digest.clone(),
                },
            }),
            pending: Some(PendingTransition {
                prior_digest: digest_label("previous synthetic title"),
                target_digest: digest_label(GENERATED_TITLE),
                disposition: PendingDisposition::Keep {
                    source: AppliedSource::Override { identity_digest },
                },
            }),
        }
    }

    fn sample_state(socket_digest: String) -> PersistedState {
        PersistedState {
            schema_version: SCHEMA_VERSION,
            socket_digest,
            cleanup_pending: true,
            tabs: BTreeMap::from([("tab-a".to_owned(), sample_tab_state())]),
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    fn write_owner_only(path: &Path, contents: &[u8]) {
        fs::write(path, contents).expect("write state document");
        fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))
            .expect("set state document mode");
    }

    #[test]
    fn creates_owner_only_directory_and_file() {
        let temporary = tempdir().expect("temporary directory");
        let (file, state) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");

        assert_eq!(mode(&temporary.path().join("tab-name")), DIRECTORY_MODE);
        file.persist(&state).expect("persist");
        assert_eq!(mode(file.path()), FILE_MODE);
        assert_eq!(fs::metadata(file.path()).expect("metadata").uid(), unsafe {
            libc::geteuid()
        });
    }

    #[test]
    fn scopes_files_by_raw_socket_path() {
        let temporary = tempdir().expect("temporary directory");
        let (first, first_state) =
            StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");
        let (second, second_state) =
            StateFile::open(temporary.path(), Path::new(SOCKET_B)).expect("open");

        assert_ne!(first.path(), second.path());
        assert_ne!(first_state.socket_digest, second_state.socket_digest);
        assert!(first_state.socket_digest != SOCKET_A);
        assert!(second_state.socket_digest != SOCKET_B);
    }

    #[test]
    fn replaces_and_loads_state_atomically() {
        let temporary = tempdir().expect("temporary directory");
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");
        let state = sample_state(initial.socket_digest);

        file.persist(&state).expect("first persist");
        file.persist(&state).expect("replacement persist");
        let (_, loaded) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("reload");

        assert!(loaded == state);
        assert!(
            fs::read_dir(temporary.path().join("tab-name"))
                .expect("state directory")
                .all(|entry| {
                    entry
                        .expect("directory entry")
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".json")
                })
        );
    }

    #[test]
    fn persisted_json_keeps_only_permitted_plaintext() {
        let temporary = tempdir().expect("temporary directory");
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");
        let state = sample_state(initial.socket_digest);

        file.persist(&state).expect("persist");
        let serialized = fs::read_to_string(file.path()).expect("serialized state");

        assert!(serialized.contains("Manual baseline label"));
        assert!(serialized.contains("Manual override label"));
        assert!(!serialized.contains(GENERATED_TITLE));
        assert!(!serialized.contains(RAW_IDENTITY));
        assert!(!serialized.contains(SOCKET_A));
    }

    #[test]
    fn malformed_and_future_documents_are_unchanged() {
        let temporary = tempdir().expect("temporary directory");
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");

        write_owner_only(file.path(), b"{");
        let malformed = fs::read(file.path()).expect("malformed contents");
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::Malformed)
        ));
        assert!(fs::read(file.path()).expect("preserved malformed contents") == malformed);

        let mut future = PersistedState::empty(initial.socket_digest);
        future.schema_version = SCHEMA_VERSION + 1;
        let future = serde_json::to_vec(&future).expect("future document");
        write_owner_only(file.path(), &future);
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsupportedVersion)
        ));
        assert!(fs::read(file.path()).expect("preserved future contents") == future);
    }

    #[test]
    fn rejects_mismatched_socket_digest_and_unsafe_file_mode() {
        let temporary = tempdir().expect("temporary directory");
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");
        let mut mismatched = sample_state(initial.socket_digest.clone());
        mismatched.socket_digest = digest_label("different socket");
        let mismatched = serde_json::to_vec(&mismatched).expect("mismatched document");
        write_owner_only(file.path(), &mismatched);

        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::SocketMismatch)
        ));
        assert!(fs::read(file.path()).expect("preserved mismatch") == mismatched);

        let state = sample_state(initial.socket_digest);
        fs::set_permissions(file.path(), fs::Permissions::from_mode(FILE_MODE)).expect("safe mode");
        file.persist(&state).expect("persist");
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644)).expect("unsafe mode");
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsafeFile)
        ));
    }

    #[test]
    fn detects_temporary_entry_replacement_before_rename() {
        let temporary = tempdir().expect("temporary directory");
        let (file, _) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");
        let (name, original) = file.create_temporary_file().expect("temporary state file");
        let path = temporary
            .path()
            .join("tab-name")
            .join(name.to_string_lossy().as_ref());
        fs::remove_file(&path).expect("unlink original entry");
        write_owner_only(&path, b"replacement");

        assert!(matches!(
            file.verify_entry_matches(&name, &original),
            Err(StateError::Rename)
        ));
    }

    #[test]
    fn rejects_directory_path_replacement_after_open() {
        let temporary = tempdir().expect("temporary directory");
        let (file, state) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");
        let directory = temporary.path().join("tab-name");
        fs::rename(&directory, temporary.path().join("old-tab-name")).expect("move directory");
        fs::create_dir(&directory).expect("replacement directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(DIRECTORY_MODE))
            .expect("replacement mode");

        assert!(matches!(
            file.persist(&state),
            Err(StateError::UnsafeDirectory)
        ));
        assert!(
            fs::read_dir(&directory)
                .expect("replacement directory")
                .next()
                .is_none()
        );
    }

    #[test]
    fn rejects_symlinked_directory_and_state_file() {
        let temporary = tempdir().expect("temporary directory");
        let redirected = tempdir().expect("redirected directory");
        symlink(redirected.path(), temporary.path().join("tab-name")).expect("directory symlink");
        assert!(matches!(
            StateFile::open(temporary.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsafeDirectory)
        ));

        let isolated = tempdir().expect("isolated directory");
        let (file, _) = StateFile::open(isolated.path(), Path::new(SOCKET_A)).expect("open");
        let target = isolated.path().join("other-state");
        fs::write(&target, b"synthetic").expect("target");
        symlink(&target, file.path()).expect("file symlink");
        assert!(matches!(
            StateFile::open(isolated.path(), Path::new(SOCKET_A)),
            Err(StateError::UnsafeFile)
        ));
    }

    #[test]
    fn round_trips_pending_transition_and_removes_file() {
        let temporary = tempdir().expect("temporary directory");
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");
        let state = sample_state(initial.socket_digest);

        file.persist(&state).expect("persist");
        let (_, loaded) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("reload");
        assert!(loaded.tabs["tab-a"].pending == state.tabs["tab-a"].pending);

        file.remove().expect("remove");
        assert!(!file.path().exists());
        let (_, empty) =
            StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("empty reload");
        assert!(empty.tabs.is_empty());
    }

    #[test]
    fn rejects_invalid_nested_digest_and_keeps_error_content_free() {
        let temporary = tempdir().expect("temporary directory");
        let (file, initial) = StateFile::open(temporary.path(), Path::new(SOCKET_A)).expect("open");
        let mut state = sample_state(initial.socket_digest);
        state.tabs.get_mut("tab-a").expect("tab").applied = Some(Applied {
            target_digest: "not-a-digest".to_owned(),
            source: AppliedSource::Baseline,
        });
        let invalid = serde_json::to_vec(&state).expect("invalid document");
        write_owner_only(file.path(), &invalid);

        let error = match StateFile::open(temporary.path(), Path::new(SOCKET_A)) {
            Err(error) => error,
            Ok(_) => panic!("invalid digest state loaded"),
        };
        assert!(error == StateError::Malformed);
        let message = error.to_string();
        assert!(!message.contains("not-a-digest"));
        assert!(!message.contains(SOCKET_A));
    }
}
