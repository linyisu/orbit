use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, fchmod, openat, renameat, renameat_with, unlinkat,
};
use std::{ffi::OsStr, fs::File, io, os::unix::fs::MetadataExt};

pub(super) type Identity = (u64, u64);
pub(super) type Permissions = u32;

pub(super) fn identity(file: &File) -> io::Result<Identity> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

pub(super) fn identity_bytes(identity: &Identity) -> Vec<u8> {
    [identity.0.to_be_bytes(), identity.1.to_be_bytes()].concat()
}

pub(super) fn create_directory(parent: &File, name: &OsStr) -> io::Result<()> {
    rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o755)).map_err(Into::into)
}

pub(super) fn permissions(file: &File) -> io::Result<Permissions> {
    Ok(file.metadata()?.mode())
}

pub(super) fn apply_permissions(file: &File, permissions: &Permissions) -> io::Result<()> {
    fchmod(file, Mode::from_bits_truncate((permissions & 0o777) as _)).map_err(Into::into)
}

pub(super) fn create_private(parent: &File, name: &OsStr) -> io::Result<File> {
    openat(
        parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
    .map_err(Into::into)
}

pub(super) fn remove_file(parent: &File, name: &OsStr) -> io::Result<()> {
    unlinkat(parent, name, AtFlags::empty()).map_err(Into::into)
}

pub(super) fn publish(
    parent: &File,
    _file: &File,
    source: &OsStr,
    target: &OsStr,
    mode: super::Replacement,
) -> io::Result<()> {
    match mode {
        super::Replacement::Create => {
            renameat_with(parent, source, parent, target, RenameFlags::NOREPLACE)
        }
        super::Replacement::Replace => renameat(parent, source, parent, target),
    }
    .map_err(Into::into)
}

pub(super) fn sync_directory(parent: &File) -> io::Result<()> {
    parent.sync_all()
}

pub(super) fn discard_unpublished(parent: &File, name: &OsStr, _file: &File) -> io::Result<()> {
    remove_file(parent, name)
}
