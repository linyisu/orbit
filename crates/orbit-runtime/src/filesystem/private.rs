//! Private Windows storage, validated using the opened object's owner and DACL.

use std::{
    ffi::c_void,
    fs::File,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION, GetAce, GetTokenInformation, INHERIT_ONLY_ACE,
        OWNER_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
        OPEN_EXISTING,
    },
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: each allocation originates from a Windows API documented to use LocalFree.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

unsafe fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: callers retain the token or security descriptor containing this valid SID.
    check(unsafe { ConvertSidToStringSidW(sid, &mut text) })?;
    let allocation = LocalAllocation(text.cast());
    let mut length = 0;
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated UTF-16 allocation.
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        drop(allocation);
        Ok(result)
    }
}

/// Return the current process token's user SID, not an environment-supplied identity.
pub fn user_id() -> io::Result<String> {
    let mut token = ptr::null_mut();
    // SAFETY: the pseudo process handle is valid and token points to writable handle storage.
    check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
    // SAFETY: OpenProcessToken transferred ownership of a new handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut length = 0;
    // SAFETY: the zero-length call only retrieves the required buffer size.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut length,
        );
    }
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    // SAFETY: the aligned allocation has at least length writable bytes and token remains live.
    check(unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    })?;
    // SAFETY: successful TokenUser returns a TOKEN_USER followed by its SID in the retained buffer.
    unsafe {
        sid_text(
            (*(buffer.as_ptr().cast::<TOKEN_USER>()))
                .User
                .Sid,
        )
    }
}

fn security() -> io::Result<LocalAllocation> {
    let sid = user_id()?;
    security_descriptor(&format!("O:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})"))
}

fn security_descriptor(sddl: &str) -> io::Result<LocalAllocation> {
    let sddl: Vec<u16> = sddl
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: sddl is NUL-terminated; descriptor receives an allocation owned by the caller.
    check(unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    })?;
    Ok(LocalAllocation(descriptor))
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in private path",
        ));
    }
    value.push(0);
    Ok(value)
}

/// Create a directory with a protected inheritable DACL, then validate existing or new storage.
pub fn create_directory(path: &Path) -> io::Result<()> {
    let descriptor = security()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let path_wide = wide(path)?;
    // SAFETY: path and security descriptor remain live; handles are not inherited.
    if unsafe { CreateDirectoryW(path_wide.as_ptr(), &attributes) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    validate_directory(path)
}

/// Allocate a temporary file with explicit ownership before any private bytes are written.
pub fn temporary_file(parent: &Path, prefix: &str) -> io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(prefix)
        .make_in(parent, |path| open_file(path, true, true))
}

/// Allocate a temporary directory and secure its empty contents before returning it.
pub fn temporary_directory(parent: &Path, prefix: &str) -> io::Result<tempfile::TempDir> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Security::{
        Authorization::SetSecurityInfo, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC, WRITE_OWNER};
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)?;
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | WRITE_OWNER)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory.path())?;
    let descriptor = security()?;
    let mut owner = ptr::null_mut();
    let mut acl = ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: descriptor owns a valid security descriptor; output pointers remain live.
    unsafe {
        check(GetSecurityDescriptorOwner(
            descriptor.0,
            &mut owner,
            &mut defaulted,
        ))?;
        check(GetSecurityDescriptorDacl(
            descriptor.0,
            &mut present,
            &mut acl,
            &mut defaulted,
        ))?;
        let error = SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            ptr::null_mut(),
            acl,
            ptr::null_mut(),
        );
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
    validate_directory(directory.path())?;
    Ok(directory)
}

/// Create missing parents privately; existing ancestors retain their permissions.
/// The requested directory itself must satisfy the private-storage contract.
pub fn create_directory_all(path: &Path) -> io::Result<()> {
    match create_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or(error)?;
            create_directory_all(parent)?;
            create_directory(path)
        }
        Err(error) => Err(error),
    }
}

/// Reject aliases, foreign ownership, and effective allow ACEs for unrelated principals.
pub fn validate_directory(path: &Path) -> io::Result<()> {
    let file = open(path, false, false, true)?;
    validate(&file, true)
}

/// Open a private regular file without following a final reparse point or accepting hardlinks.
pub fn open_file(path: &Path, create: bool, exclusive: bool) -> io::Result<File> {
    let file = open(path, create, exclusive, false)?;
    validate(&file, false)?;
    Ok(file)
}

/// Open a validated private log in append mode, without following final reparse points.
pub fn open_append(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let created = open_file(path, true, false)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    validate(&file, false)?;
    if identity(&file)? != identity(&created)? {
        return Err(io::Error::other("private log changed during open"));
    }
    Ok(file)
}

fn open(path: &Path, create: bool, exclusive: bool, directory: bool) -> io::Result<File> {
    let descriptor = security()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let path = wide(path)?;
    let disposition = if directory {
        OPEN_EXISTING
    } else if exclusive {
        CREATE_NEW
    } else if create {
        OPEN_ALWAYS
    } else {
        OPEN_EXISTING
    };
    // SAFETY: buffers and security attributes live through the synchronous open.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_GENERIC_READ | if create { FILE_GENERIC_WRITE } else { 0 },
            FILE_SHARE_READ | FILE_SHARE_WRITE | if directory { 0 } else { FILE_SHARE_DELETE },
            &attributes,
            disposition,
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a new owned file handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn validate(file: &File, directory: bool) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        }
    {
        return Err(denied());
    }
    if !directory && cap_fs_ext::MetadataExt::nlink(&cap_std::fs::Metadata::from_file(file)?) != 1 {
        return Err(denied());
    }
    let mut owner = ptr::null_mut();
    let mut acl: *mut ACL = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    // SAFETY: the handle is live and all output pointers target writable storage.
    let error = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let _allocation = LocalAllocation(descriptor);
    let current = user_id()?;
    // SAFETY: owner and ACL point into the retained GetSecurityInfo allocation.
    unsafe {
        if owner.is_null() || acl.is_null() || sid_text(owner)? != current {
            return Err(denied());
        }
        for index in 0..(*acl).AceCount {
            let mut ace = ptr::null_mut();
            check(GetAce(acl, index as u32, &mut ace))?;
            let header = &*ace.cast::<ACE_HEADER>();
            if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
                || u32::from(header.AceType) == ACCESS_DENIED_ACE_TYPE
            {
                continue;
            }
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
                return Err(denied());
            }
            let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            let sid = sid_text(
                ptr::from_ref(&allowed.SidStart)
                    .cast_mut()
                    .cast(),
            )?;
            if allowed.Mask != 0 && sid != current && sid != "S-1-5-18" && sid != "S-1-5-32-544" {
                return Err(denied());
            }
        }
    }
    Ok(())
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "storage must be private and owned by this user",
    )
}

/// Flush directory changes where the filesystem supports it; never report a failed flush as saved.
pub fn sync_directory(path: &Path) -> io::Result<()> {
    open(path, true, false, true)?.sync_all()
}

/// Compare complete native file identities, including 128-bit identifiers used by ReFS.
pub fn identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the handle is live and the output buffer has the size required by FileIdInfo.
    check(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    })?;
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_storage_rejects_links_and_preserves_identity() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("private");
        create_directory(&directory).unwrap();
        validate_directory(&directory).unwrap();
        let path = directory.join("record");
        let file = open_file(&path, true, true).unwrap();
        let reopened = open_file(&path, false, false).unwrap();
        assert_eq!(identity(&file).unwrap(), identity(&reopened).unwrap());
        assert!(open_file(&path, true, true).is_err());
        let alias = directory.join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert_eq!(
            open_file(&path, false, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let link = root.path().join("linked-directory");
        std::os::windows::fs::symlink_dir(&directory, &link).unwrap();
        assert_eq!(
            validate_directory(&link)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn unrelated_reader_is_not_accepted_as_private_storage() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("public");
        let sid = user_id().unwrap();
        let descriptor =
            security_descriptor(&format!("O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;GR;;;WD)")).unwrap();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // SAFETY: the temporary path and descriptor remain live for this synchronous call.
        check(unsafe { CreateDirectoryW(wide(&path).unwrap().as_ptr(), &attributes) }).unwrap();
        assert_eq!(
            validate_directory(&path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(create_directory(&path).is_err());
    }
}
