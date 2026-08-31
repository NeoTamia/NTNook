//! Small operating-system primitives shared by persistence and installers.

use std::io;
use std::path::Path;

#[cfg(unix)]
use std::fs;

#[cfg(unix)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn sync_directory(_path: &Path) -> io::Result<()> {
    // Windows does not allow opening a directory with the portable File API.
    // MoveFileExW with MOVEFILE_WRITE_THROUGH provides the durability boundary.
    Ok(())
}
