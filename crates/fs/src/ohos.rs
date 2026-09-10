//! OHOS file access for picked documents.
//!
//! The document picker hands back `file://docs` URIs and descriptors rather than
//! ordinary paths, and the HAP sandbox cannot open those paths directly, so
//! paths that come out of the picker are tagged:
//!
//! * `dir:<uri>`  - a picked directory, listed through the host.
//! * `file:<uri>` - a file inside a picked directory, opened by URI.
//! * `fd:<fd>`    - a file the host already opened for the picker.
//!
//! `OhosFs` decodes those tags and delegates everything else to the real
//! filesystem. The host entry points are injected at startup so this crate does
//! not depend on the platform backend. They are asynchronous because the host
//! bridge enters the ArkUI JavaScript VM, which only the UI thread may do.

use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, OnceLock},
};

use anyhow::{Result, anyhow};
use async_tar::Archive;
use futures::{AsyncRead, Stream, future::BoxFuture, stream};
use git::repository::GitRepository;
use rope::Rope;
use text::LineEnding;

use crate::{
    CopyOptions, CreateOptions, FileHandle, Fs, JobEventReceiver, MTime, Metadata, PathEvent,
    RemoveOptions, RenameOptions, TrashId, TrashRestoreError, Watcher,
};

const DIR_PREFIX: &str = "dir:";
const FILE_PREFIX: &str = "file:";
const FD_PREFIX: &str = "fd:";

/// Host entry points the wrapper needs, registered by the OHOS entry point.
#[derive(Clone, Copy)]
pub struct OhosFsBridge {
    pub list_dir: fn(String) -> BoxFuture<'static, Vec<(String, bool, String)>>,
    pub open_file: fn(String) -> BoxFuture<'static, Option<i32>>,
    pub read_fd: fn(i32) -> BoxFuture<'static, io::Result<Vec<u8>>>,
    pub write_fd: fn(i32, Vec<u8>) -> BoxFuture<'static, io::Result<()>>,
}

static BRIDGE: OnceLock<OhosFsBridge> = OnceLock::new();

pub fn set_ohos_fs_bridge(bridge: OhosFsBridge) {
    let _ = BRIDGE.set(bridge);
}

fn bridge() -> Result<&'static OhosFsBridge> {
    BRIDGE
        .get()
        .ok_or_else(|| anyhow!("the OHOS filesystem bridge is not installed"))
}

fn tagged<'a>(path: &'a Path, prefix: &str) -> Option<&'a str> {
    path.to_str()?.strip_prefix(prefix)
}

fn directory_uri(path: &Path) -> Option<&str> {
    tagged(path, DIR_PREFIX)
}

fn file_uri(path: &Path) -> Option<&str> {
    tagged(path, FILE_PREFIX)
}

fn descriptor(path: &Path) -> Option<i32> {
    tagged(path, FD_PREFIX)?.parse().ok()
}

fn is_picked_file(path: &Path) -> bool {
    file_uri(path).is_some() || descriptor(path).is_some()
}

fn picked_metadata(is_dir: bool) -> Metadata {
    Metadata {
        inode: 0,
        mtime: MTime::from_seconds_and_nanos(0, 0),
        is_symlink: false,
        is_dir,
        len: 0,
        is_fifo: false,
        is_executable: false,
        is_writable: true,
    }
}

async fn read_picked(path: &Path) -> Result<Vec<u8>> {
    if let Some(uri) = file_uri(path) {
        let fd = (bridge()?.open_file)(uri.to_string())
            .await
            .ok_or_else(|| anyhow!("could not open {uri}"))?;
        return Ok((bridge()?.read_fd)(fd).await?);
    }
    if let Some(fd) = descriptor(path) {
        return Ok((bridge()?.read_fd)(fd).await?);
    }
    Err(anyhow!("{} is not a picked file", path.display()))
}

async fn write_picked(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(uri) = file_uri(path) {
        let fd = (bridge()?.open_file)(uri.to_string())
            .await
            .ok_or_else(|| anyhow!("could not open {uri}"))?;
        return Ok((bridge()?.write_fd)(fd, bytes.to_vec()).await?);
    }
    if let Some(fd) = descriptor(path) {
        return Ok((bridge()?.write_fd)(fd, bytes.to_vec()).await?);
    }
    Err(anyhow!("{} is not a picked file", path.display()))
}

#[derive(Debug)]
struct PickedFileHandle {
    path: PathBuf,
}

impl FileHandle for PickedFileHandle {
    fn current_path(&self, _fs: &Arc<dyn Fs>) -> Result<PathBuf> {
        Ok(self.path.clone())
    }
}

struct NullWatcher;

impl Watcher for NullWatcher {
    fn add(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    fn remove(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
}

/// Tagged paths are read and written through the host; everything else is the
/// real filesystem.
pub struct OhosFs {
    inner: Arc<dyn Fs>,
}

impl OhosFs {
    pub fn new(inner: Arc<dyn Fs>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl Fs for OhosFs {
    async fn create_dir(&self, path: &Path) -> Result<()> {
        self.inner.create_dir(path).await
    }

    async fn create_symlink(&self, path: &Path, target: PathBuf) -> Result<()> {
        self.inner.create_symlink(path, target).await
    }

    async fn create_file(&self, path: &Path, options: CreateOptions) -> Result<()> {
        if is_picked_file(path) {
            write_picked(path, &[]).await
        } else {
            self.inner.create_file(path, options).await
        }
    }

    async fn create_file_with(
        &self,
        path: &Path,
        content: Pin<&mut (dyn AsyncRead + Send)>,
    ) -> Result<()> {
        self.inner.create_file_with(path, content).await
    }

    async fn extract_tar_file(
        &self,
        path: &Path,
        content: Archive<Pin<&mut (dyn AsyncRead + Send)>>,
    ) -> Result<()> {
        self.inner.extract_tar_file(path, content).await
    }

    async fn copy_file(&self, source: &Path, target: &Path, options: CopyOptions) -> Result<()> {
        self.inner.copy_file(source, target, options).await
    }

    async fn rename(&self, source: &Path, target: &Path, options: RenameOptions) -> Result<()> {
        self.inner.rename(source, target, options).await
    }

    async fn remove_dir(&self, path: &Path, options: RemoveOptions) -> Result<()> {
        self.inner.remove_dir(path, options).await
    }

    async fn trash(&self, path: &Path, options: RemoveOptions) -> Result<TrashId> {
        self.inner.trash(path, options).await
    }

    async fn remove_file(&self, path: &Path, options: RemoveOptions) -> Result<()> {
        self.inner.remove_file(path, options).await
    }

    async fn open_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>> {
        if is_picked_file(path) {
            Ok(Arc::new(PickedFileHandle {
                path: path.to_path_buf(),
            }))
        } else {
            self.inner.open_handle(path).await
        }
    }

    async fn open_sync(&self, path: &Path) -> Result<Box<dyn io::Read + Send + Sync>> {
        if is_picked_file(path) {
            let bytes = read_picked(path).await?;
            Ok(Box::new(io::Cursor::new(bytes)))
        } else {
            self.inner.open_sync(path).await
        }
    }

    async fn load_bytes(&self, path: &Path) -> Result<Vec<u8>> {
        if is_picked_file(path) {
            read_picked(path).await
        } else {
            self.inner.load_bytes(path).await
        }
    }

    async fn atomic_write(&self, path: PathBuf, text: String) -> Result<()> {
        if is_picked_file(&path) {
            write_picked(&path, text.as_bytes()).await
        } else {
            self.inner.atomic_write(path, text).await
        }
    }

    async fn save(&self, path: &Path, text: &Rope, line_ending: LineEnding) -> Result<()> {
        if is_picked_file(path) {
            let mut bytes = Vec::new();
            for chunk in text::chunks_with_line_ending(text, line_ending) {
                bytes.extend_from_slice(chunk.as_bytes());
            }
            write_picked(path, &bytes).await
        } else {
            self.inner.save(path, text, line_ending).await
        }
    }

    async fn write(&self, path: &Path, content: &[u8]) -> Result<()> {
        if is_picked_file(path) {
            write_picked(path, content).await
        } else {
            self.inner.write(path, content).await
        }
    }

    async fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        if directory_uri(path).is_some() || is_picked_file(path) {
            return Ok(path.to_path_buf());
        }
        self.inner.canonicalize(path).await
    }

    async fn is_file(&self, path: &Path) -> bool {
        if directory_uri(path).is_some() {
            return false;
        }
        if is_picked_file(path) {
            return true;
        }
        self.inner.is_file(path).await
    }

    async fn is_dir(&self, path: &Path) -> bool {
        if directory_uri(path).is_some() {
            return true;
        }
        if is_picked_file(path) {
            return false;
        }
        self.inner.is_dir(path).await
    }

    async fn metadata(&self, path: &Path) -> Result<Option<Metadata>> {
        if directory_uri(path).is_some() {
            return Ok(Some(picked_metadata(true)));
        }
        if is_picked_file(path) {
            return Ok(Some(picked_metadata(false)));
        }
        self.inner.metadata(path).await
    }

    async fn read_link(&self, path: &Path) -> Result<PathBuf> {
        self.inner.read_link(path).await
    }

    async fn read_dir(
        &self,
        path: &Path,
    ) -> Result<Pin<Box<dyn Send + Stream<Item = Result<PathBuf>>>>> {
        let Some(uri) = directory_uri(path) else {
            return self.inner.read_dir(path).await;
        };
        let entries = (bridge()?.list_dir)(uri.to_string()).await;
        let children = entries.into_iter().map(|(_, is_dir, child_uri)| {
            let prefix = if is_dir { DIR_PREFIX } else { FILE_PREFIX };
            Ok(PathBuf::from(format!("{prefix}{child_uri}")))
        });
        Ok(Box::pin(stream::iter(children)))
    }

    async fn watch(
        &self,
        path: &Path,
        latency: std::time::Duration,
    ) -> (
        Pin<Box<dyn Send + Stream<Item = Vec<PathEvent>>>>,
        Arc<dyn Watcher>,
    ) {
        if directory_uri(path).is_some() || is_picked_file(path) {
            return (Box::pin(stream::empty()), Arc::new(NullWatcher));
        }
        self.inner.watch(path, latency).await
    }

    fn open_repo(
        &self,
        abs_dot_git: &Path,
        system_git_binary_path: Option<&Path>,
    ) -> Result<Arc<dyn GitRepository>> {
        self.inner.open_repo(abs_dot_git, system_git_binary_path)
    }

    async fn git_init(
        &self,
        abs_work_directory: &Path,
        fallback_branch_name: String,
    ) -> Result<()> {
        self.inner
            .git_init(abs_work_directory, fallback_branch_name)
            .await
    }

    async fn git_clone(&self, abs_work_directory: &Path, repo_url: &str) -> Result<()> {
        self.inner.git_clone(abs_work_directory, repo_url).await
    }

    async fn git_config(&self, abs_work_directory: &Path, args: Vec<String>) -> Result<String> {
        self.inner.git_config(abs_work_directory, args).await
    }

    fn is_fake(&self) -> bool {
        self.inner.is_fake()
    }

    async fn is_case_sensitive(&self) -> bool {
        self.inner.is_case_sensitive().await
    }

    fn subscribe_to_jobs(&self) -> JobEventReceiver {
        self.inner.subscribe_to_jobs()
    }

    fn original_path_for_trash_id(&self, trash_id: TrashId) -> Option<PathBuf> {
        self.inner.original_path_for_trash_id(trash_id)
    }

    async fn restore(&self, trash_id: TrashId) -> std::result::Result<PathBuf, TrashRestoreError> {
        self.inner.restore(trash_id).await
    }
}
