use std::{ffi::OsString, fs::File, io, path::Path};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use windows_sys::Win32::Foundation::ERROR_NOT_A_REPARSE_POINT;

// Every directory originates from cap-std, which excludes FILE_SHARE_DELETE.
// Cloning preserves that handle contract; no path-based reopen is used here.
fn directory(file: &File) -> io::Result<Dir> {
    Ok(Dir::from_std_file(file.try_clone()?))
}

pub(crate) fn open_root(path: &Path) -> io::Result<File> {
    Dir::open_ambient_dir(path, cap_std::ambient_authority()).map(Dir::into_std_file)
}

pub(crate) fn open_directory(parent: &File, name: &Path) -> io::Result<File> {
    directory(parent)?
        .open_dir_nofollow(name)
        .map(Dir::into_std_file)
}

pub(crate) fn open_read(parent: &File, name: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No);
    directory(parent)?
        .open_with(name, &options)
        .map(|file| file.into_std())
}

pub(crate) fn read_link(parent: &File, name: &Path) -> io::Result<Option<std::path::PathBuf>> {
    match directory(parent)?.read_link_contents(name) {
        Ok(target) => Ok(Some(target)),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(ERROR_NOT_A_REPARSE_POINT as i32) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn entries(file: &File) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in directory(file)?.entries()? {
        names.push(entry?.file_name());
        if names.len() > 10_000 {
            return Err(io::Error::other("directory exceeds 10000 entries"));
        }
    }
    names.sort();
    Ok(names)
}

pub(crate) fn same_file(first: &File, second: &File) -> io::Result<bool> {
    Ok(crate::filesystem::private::identity(first)?
        == crate::filesystem::private::identity(second)?)
}
