use std::{ffi::OsString, fs::File, io, os::unix::ffi::OsStringExt, path::Path};

use rustix::fs::{Mode, OFlags, openat};

pub(crate) fn open_root(path: &Path) -> io::Result<File> {
    File::open(path)
}

pub(crate) fn open_directory(parent: &File, name: &Path) -> io::Result<File> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

pub(crate) fn open_read(parent: &File, name: &Path) -> io::Result<File> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

pub(crate) fn read_link(parent: &File, name: &Path) -> io::Result<Option<std::path::PathBuf>> {
    match rustix::fs::readlinkat(parent, name, Vec::new()) {
        Ok(target) => Ok(Some(OsString::from_vec(target.into_bytes()).into())),
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn entries(directory: &File) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(directory)? {
        let name = OsString::from_vec(entry?.file_name().to_bytes().to_owned());
        if name != "." && name != ".." {
            names.push(name);
        }
        if names.len() > 10_000 {
            return Err(io::Error::other("directory exceeds 10000 entries"));
        }
    }
    names.sort();
    Ok(names)
}

pub(crate) fn same_file(first: &File, second: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let first = first.metadata()?;
    let second = second.metadata()?;
    Ok(first.dev() == second.dev() && first.ino() == second.ino())
}
