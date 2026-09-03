//! File operations for terminals.
//!
//! Local shells operate on the server's own filesystem via `std::fs`; remote
//! SSH shells use the SFTP subsystem through `openssh-sftp-client` over a
//! russh channel (see `pool.rs`).

use std::path::Path;

mod local;
mod pool;
mod reader;
mod remote;
mod zip;

/// Maximum number of entries returned per directory listing. A huge listing
/// serializes into a multi-megabyte CBOR message that freezes the browser when
/// decoded; entries beyond this cap are omitted and the caller is told the
/// list is truncated.
pub const MAX_LIST_ENTRIES: usize = 20000;

/// The final path component of `path`, falling back to the path itself.
///
/// Shared by the ZIP archive walkers (archive entry naming) and the download
/// handler (Content-Disposition filename).
pub fn file_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

pub use local::{
    copy_local, list_local, mkdir_local, read_local, remove_local, rename_local, write_at_local,
    write_local,
};
pub use pool::{open_sftp_probe, SftpConnectError, SftpPool};
pub use reader::{reader_local, reader_remote, size_local, size_remote, DownloadReader};
pub use remote::{
    copy_remote, list_remote, mkdir_remote, read_remote, remove_remote, rename_remote,
    write_at_remote, write_remote,
};
pub use zip::{archive_local_stream, archive_remote_stream};
