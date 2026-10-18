use std::{
    ffi::OsStr,
    fs::File,
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle},
    },
    ptr,
};

use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_INFORMATION,
            FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION,
            FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
            FILE_SYNCHRONOUS_IO_NONALERT, FileDispositionInformation, FileRenameInformationEx,
            NtCreateFile, NtSetInformationFile,
        },
    },
    Win32::{
        Foundation::{OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING},
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
                SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
            UNPROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            DELETE, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SYSTEM,
            FILE_BASIC_INFO, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo, SYNCHRONIZE,
            SetFileInformationByHandle, WRITE_DAC,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

use super::super::private::{self, LocalAllocation};

pub(super) type Identity = (u64, [u8; 16]);

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Permissions {
    dacl: String,
    attributes: u32,
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn status(result: i32) -> io::Result<()> {
    if result < 0 {
        // SAFETY: the API translates an NTSTATUS value without dereferencing memory.
        Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(result) } as i32,
        ))
    } else {
        Ok(())
    }
}

pub(super) fn identity(file: &File) -> io::Result<Identity> {
    private::identity(file)
}

pub(super) fn permissions(file: &File) -> io::Result<Permissions> {
    const PRESERVED_ATTRIBUTES: u32 = FILE_ATTRIBUTE_NORMAL
        | FILE_ATTRIBUTE_READONLY
        | FILE_ATTRIBUTE_HIDDEN
        | FILE_ATTRIBUTE_SYSTEM
        | FILE_ATTRIBUTE_ARCHIVE
        | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED;
    let attributes = file.metadata()?.file_attributes();
    if attributes & !PRESERVED_ATTRIBUTES != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic replacement cannot preserve this file's native storage attributes",
        ));
    }
    let mut descriptor = ptr::null_mut();
    // SAFETY: the live file permits attribute queries; descriptor receives a LocalFree allocation.
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let descriptor = LocalAllocation(descriptor);
    let mut text = ptr::null_mut();
    let mut length = 0;
    // SAFETY: the descriptor is retained and both output locations are writable.
    check(unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor.0,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            &mut length,
        )
    })?;
    let allocation = LocalAllocation(text.cast());
    // SAFETY: the API returned length UTF-16 units including the trailing NUL.
    let text = unsafe { std::slice::from_raw_parts(text, length as usize) };
    let dacl = String::from_utf16(text.strip_suffix(&[0]).unwrap_or(text))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    drop(allocation);
    Ok(Permissions { dacl, attributes })
}

pub(super) fn apply_permissions(file: &File, permissions: &Permissions) -> io::Result<()> {
    let descriptor = private::security_descriptor(&permissions.dacl)?;
    let mut acl = ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: descriptor owns a valid parsed descriptor and all output pointers are live.
    unsafe {
        check(GetSecurityDescriptorDacl(
            descriptor.0,
            &mut present,
            &mut acl,
            &mut defaulted,
        ))?;
        check(GetSecurityDescriptorControl(
            descriptor.0,
            &mut control,
            &mut revision,
        ))?;
        let inheritance = if control & SE_DACL_PROTECTED != 0 {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        let result = SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | inheritance,
            ptr::null_mut(),
            ptr::null_mut(),
            acl,
            ptr::null_mut(),
        );
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
    }
    let basic = FILE_BASIC_INFO {
        FileAttributes: permissions.attributes,
        ..Default::default()
    };
    // SAFETY: the unpublished file has write-attribute access; the structure has the documented size.
    check(unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileBasicInfo,
            ptr::from_ref(&basic).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    })
}

fn open_at(parent: &File, name: &OsStr, create: bool) -> io::Result<File> {
    let mut name: Vec<u16> = name.encode_wide().collect();
    let bytes = u16::try_from(name.len() * size_of::<u16>())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name is too long"))?;
    let unicode = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name.as_mut_ptr(),
    };
    let security = if create {
        Some(private::security()?)
    } else {
        None
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: security
            .as_ref()
            .map_or(ptr::null(), |value| value.0.cast()),
        ..Default::default()
    };
    let mut handle = ptr::null_mut();
    let mut io_status = IO_STATUS_BLOCK::default();
    let access = DELETE
        | SYNCHRONIZE
        | if create {
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC
        } else {
            0
        };
    // SAFETY: the parent and name are retained; exclusive creation receives a private descriptor
    // before any other principal can open the file. The relative name is one validated component.
    status(unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &attributes,
            &mut io_status,
            ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            if create { FILE_CREATE } else { FILE_OPEN },
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    })?;
    // SAFETY: successful NtCreateFile transfers a newly owned handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub(super) fn create_private(parent: &File, name: &OsStr) -> io::Result<File> {
    open_at(parent, name, true)
}

pub(super) fn remove_file(parent: &File, name: &OsStr) -> io::Result<()> {
    let file = open_at(parent, name, false)?;
    delete(&file)
}

fn delete(file: &File) -> io::Result<()> {
    let information = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: the live file has DELETE access; the initialized structure remains valid for the call.
    status(unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut io_status,
            ptr::from_ref(&information).cast(),
            size_of::<FILE_DISPOSITION_INFORMATION>() as u32,
            FileDispositionInformation,
        )
    })
}

pub(super) fn discard_unpublished(_parent: &File, _name: &OsStr, file: &File) -> io::Result<()> {
    let attributes = file.metadata()?.file_attributes() & !FILE_ATTRIBUTE_READONLY;
    let basic = FILE_BASIC_INFO {
        FileAttributes: if attributes == 0 {
            FILE_ATTRIBUTE_NORMAL
        } else {
            attributes
        },
        ..Default::default()
    };
    // SAFETY: the retained creation handle still owns this unpublished temporary object.
    check(unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileBasicInfo,
            ptr::from_ref(&basic).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    })?;
    delete(file)
}

pub(super) fn publish(
    parent: &File,
    file: &File,
    _source: &OsStr,
    target: &OsStr,
    mode: super::Replacement,
) -> io::Result<()> {
    let name: Vec<u16> = target.encode_wide().collect();
    let length = size_of::<FILE_RENAME_INFORMATION>() + name.len() * size_of::<u16>();
    let mut buffer = vec![
        0usize;
        length
            .max(size_of::<FILE_RENAME_INFORMATION>())
            .div_ceil(size_of::<usize>())
    ];
    let header = buffer
        .as_mut_ptr()
        .cast::<FILE_RENAME_INFORMATION>();
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: the allocation is aligned for the header and extends through all UTF-16 name units.
    // The directory remains live; RootDirectory prevents a path-based reopen during publication.
    unsafe {
        header.write(FILE_RENAME_INFORMATION::default());
        (*header).Anonymous.Flags = if matches!(mode, super::Replacement::Replace) {
            FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS
        } else {
            0
        };
        (*header).RootDirectory = parent.as_raw_handle();
        (*header).FileNameLength = (name.len() * size_of::<u16>()) as u32;
        ptr::copy_nonoverlapping(
            name.as_ptr(),
            buffer
                .as_mut_ptr()
                .cast::<u8>()
                .add(offset_of!(FILE_RENAME_INFORMATION, FileName))
                .cast(),
            name.len(),
        );
        status(NtSetInformationFile(
            file.as_raw_handle(),
            &mut io_status,
            buffer.as_ptr().cast(),
            length as u32,
            FileRenameInformationEx,
        ))
    }
}

pub(super) fn sync_directory(parent: &File) -> io::Result<()> {
    let name = UNICODE_STRING::default();
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &name,
        ..Default::default()
    };
    let mut handle = ptr::null_mut();
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: an empty relative name reopens the retained directory object itself. The explicit
    // directory option avoids ReOpenFile's file-only behavior; no mutable path is looked up.
    status(unsafe {
        NtCreateFile(
            &mut handle,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            &attributes,
            &mut io_status,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    })?;
    // SAFETY: successful NtCreateFile transfers a newly owned directory handle.
    unsafe { File::from_raw_handle(handle) }.sync_all()
}
