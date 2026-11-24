//! Windows directory operations anchored to open directory handles.
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    mem::{offset_of, size_of},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, Prefix},
    ptr::{null, null_mut},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE,
            FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_INFORMATION_EX,
            FILE_DISPOSITION_POSIX_SEMANTICS, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
            FileDispositionInformationEx, FileRenameInformation, NtCreateFile,
            NtSetInformationFile,
        },
    },
    Win32::{
        Foundation::{
            GENERIC_READ, GENERIC_WRITE, HANDLE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE,
            RtlNtStatusToDosError, UNICODE_STRING,
        },
        Storage::FileSystem::{
            DELETE, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_CASE_SENSITIVE_INFO,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
            FileCaseSensitiveInfo, FileIdInfo, GetDriveTypeW, GetFileInformationByHandleEx,
            SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const DIRECTORY_ACCESS: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const MAX_NAME: usize = 255;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub(crate) fn validate_component(name: &str) -> io::Result<()> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let stem = stem.to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if name.is_empty()
        || name.encode_utf16().count() > MAX_NAME
        || name.ends_with([' ', '.'])
        || name.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*'])
        || name.chars().any(char::is_control)
        || device
    {
        return Err(invalid("unsafe Windows path component"));
    }
    Ok(())
}

fn name_utf16(name: &OsStr) -> io::Result<Vec<u16>> {
    let name = name
        .to_str()
        .ok_or_else(|| invalid("invalid Unicode path"))?;
    validate_component(name)?;
    Ok(name.encode_utf16().collect())
}

fn check_directory(handle: &File) -> io::Result<()> {
    let metadata = handle.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid("Windows path is not a plain directory"));
    }
    Ok(())
}

fn require_case_insensitive(handle: &File) -> io::Result<()> {
    let mut info = FILE_CASE_SENSITIVE_INFO::default();
    // SAFETY: the buffer matches the information class and the directory handle remains live.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileCaseSensitiveInfo,
            (&raw mut info).cast(),
            size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.Flags != 0 {
        return Err(invalid("case-sensitive directory modes are unsupported"));
    }
    Ok(())
}

fn nt_result(status: i32) -> io::Result<()> {
    if status < 0 {
        // SAFETY: this routine accepts any NTSTATUS and accesses no caller memory.
        let error = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(())
}

fn open_child(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    share: u32,
) -> io::Result<File> {
    let mut name = name_utf16(name)?;
    let bytes = (name.len() * size_of::<u16>()) as u16;
    let name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        ..Default::default()
    };
    let mut raw = null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: the name is one validated relative component. All pointers and the parent handle
    // stay live for this synchronous call. A successful call transfers the returned handle.
    let status = unsafe {
        NtCreateFile(
            &mut raw,
            access,
            &attributes,
            &mut status_block,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            share,
            disposition,
            options | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    nt_result(status)?;
    // SAFETY: NtCreateFile succeeded and returned an owned synchronous file handle.
    Ok(unsafe { File::from_raw_handle(raw) })
}

fn directory_child(parent: &File, name: &OsStr, create: bool, access: u32) -> io::Result<File> {
    let (disposition, options) = if create {
        (FILE_CREATE, FILE_DIRECTORY_FILE)
    } else {
        (FILE_OPEN, FILE_OPEN_REPARSE_POINT)
    };
    let handle = open_child(parent, name, access, disposition, options, SHARE)?;
    check_directory(&handle)?;
    Ok(handle)
}

fn child_or_create(parent: &File, name: &OsStr, create: bool) -> io::Result<File> {
    match directory_child(parent, name, false, DIRECTORY_ACCESS) {
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            match directory_child(parent, name, true, DIRECTORY_ACCESS) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    directory_child(parent, name, false, DIRECTORY_ACCESS)
                }
                result => result,
            }
        }
        result => result,
    }
}

// Keep each ancestor open without delete sharing while the caller performs its operation.
fn directory_chain(path: &Path, create: bool) -> io::Result<Vec<File>> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("invalid Unicode path"))?;
    if text
        .split(['/', '\\'])
        .any(|name| matches!(name, "." | ".."))
    {
        return Err(invalid("relative path components are unsupported"));
    }
    let mut components = path.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
            _ => return Err(invalid("a local drive-letter path is required")),
        },
        _ => return Err(invalid("an absolute drive-letter path is required")),
    };
    if components.next() != Some(Component::RootDir) {
        return Err(invalid("drive-relative path is unsupported"));
    }
    const DRIVE_FIXED: u32 = 3;
    let drive_name = [u16::from(drive), u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: the drive name is NUL-terminated, and the API only reads it.
    if unsafe { GetDriveTypeW(drive_name.as_ptr()) } != DRIVE_FIXED {
        return Err(invalid("a fixed local drive is required"));
    }
    let root = format!(r"\\?\{}:\", char::from(drive));
    // Resolve and validate all names before creating any directory.
    let names = components
        .map(|component| match component {
            Component::Normal(name) => {
                name_utf16(name)?;
                Ok(name)
            }
            _ => Err(invalid("unsafe path component")),
        })
        .collect::<io::Result<Vec<_>>>()?;
    let root = OpenOptions::new()
        .access_mode(DIRECTORY_ACCESS)
        .share_mode(SHARE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(root)?;
    check_directory(&root)?;
    let mut chain = vec![root];
    for name in names {
        chain.push(child_or_create(chain.last().unwrap(), name, create)?);
    }
    Ok(chain)
}

pub(crate) fn ensure_directory(path: &Path) -> io::Result<()> {
    directory_chain(path, true).map(|_| ())
}

/// Open an existing regular file for read, or exclusively create a new file for write.
pub(crate) fn regular_file(path: &Path, create: bool) -> io::Result<File> {
    open_regular(path, create, create, SHARE)
}

fn open_regular(path: &Path, create: bool, write: bool, share: u32) -> io::Result<File> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(invalid("file path requires a parent and a name"));
    };
    let chain = directory_chain(parent, false)?;
    let parent = chain.last().unwrap();
    let (disposition, options) = if create {
        require_case_insensitive(parent)?;
        (FILE_CREATE, FILE_NON_DIRECTORY_FILE)
    } else {
        (FILE_OPEN, FILE_OPEN_REPARSE_POINT)
    };
    let access = GENERIC_READ | if write { GENERIC_WRITE } else { 0 };
    let file = open_child(
        parent,
        name,
        access | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        disposition,
        options,
        share,
    )?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid("Windows path is not a plain regular file"));
    }
    Ok(file)
}

pub(crate) fn content_file(path: &Path, create: bool, write: bool) -> io::Result<File> {
    let file = open_regular(path, create, write, SHARE)?;
    if write {
        require_single_link(&file)?;
    }
    Ok(file)
}

fn require_single_link(file: &File) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_STANDARD_INFO, FileStandardInfo};
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: the handle is live and the buffer matches FileStandardInfo.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.NumberOfLinks != 1 {
        return Err(invalid(
            "refusing writable content handle for a linked or unlinked file",
        ));
    }
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(invalid("directory sync requires a named directory"));
    };
    let chain = directory_chain(parent, false)?;
    directory_child(
        chain.last().unwrap(),
        name,
        false,
        DIRECTORY_ACCESS | GENERIC_WRITE,
    )?
    .sync_all()
}

#[repr(C)]
struct RenameInformation {
    flags: u32,
    root: HANDLE,
    name_bytes: u32,
    name: [u16; MAX_NAME],
}

const _: () = {
    assert!(
        offset_of!(RenameInformation, root) == offset_of!(FILE_RENAME_INFORMATION, RootDirectory)
    );
    assert!(
        offset_of!(RenameInformation, name_bytes)
            == offset_of!(FILE_RENAME_INFORMATION, FileNameLength)
    );
    assert!(offset_of!(RenameInformation, name) == offset_of!(FILE_RENAME_INFORMATION, FileName));
};

#[cfg(test)]
pub(crate) fn rename_directory_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let (Some(from_parent), Some(from_name), Some(to_parent), Some(to_name)) =
        (from.parent(), from.file_name(), to.parent(), to.file_name())
    else {
        return Err(invalid("rename requires source and destination names"));
    };
    let from_chain = directory_chain(from_parent, false)?;
    let to_chain = directory_chain(to_parent, false)?;
    let target = to_chain.last().unwrap();
    require_case_insensitive(target)?;
    let source = directory_child(
        from_chain.last().unwrap(),
        from_name,
        false,
        DIRECTORY_ACCESS | DELETE,
    )?;
    rename_noreplace(&source, target, &name_utf16(to_name)?)
}

pub(crate) fn publish_verified_copy(
    from: &Path,
    to: &Path,
    expected_size: u64,
    verify: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(invalid("publication requires a destination name"));
    };
    let name = name_utf16(name)?;
    let chain = directory_chain(parent, false)?;
    let target = chain.last().unwrap();
    publish_copy_at(from, target, &name, expected_size, true, verify)
}

fn publish_copy_at(
    from: &Path,
    target: &File,
    name: &[u16],
    expected_size: u64,
    readonly: bool,
    verify: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    require_case_insensitive(target)?;
    let source = open_regular(from, false, false, FILE_SHARE_READ)?;
    if volume(&source)? != volume(target)? {
        return Err(invalid("publication requires a single volume"));
    }
    if source.metadata()?.len() != expected_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "publication source size mismatch",
        ));
    }
    let temporary_name = format!(".app-depot-publish-{}", uuid::Uuid::new_v4());
    let mut temporary = open_child(
        target,
        OsStr::new(&temporary_name),
        GENERIC_READ | GENERIC_WRITE | DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        0,
    )?;
    let result = (|| {
        if io::copy(&mut source.take(expected_size), &mut temporary)? != expected_size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "publication source shortened",
            ));
        }
        temporary.seek(SeekFrom::Start(0))?;
        verify(&temporary)?;
        if readonly {
            let mut permissions = temporary.metadata()?.permissions();
            permissions.set_readonly(true);
            temporary.set_permissions(permissions)?;
        }
        temporary.sync_all()?;
        rename_noreplace(&temporary, target, name)
    })();
    if result.is_err() {
        delete_open_file(temporary)?;
    }
    result
}

/// Publish an installation containing a single independent payload and its ownership marker.
pub(crate) fn publish_file_install(
    from: &Path,
    to: &Path,
    id: &str,
    size: u64,
    verify: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    publish_install(to, id, |stage| {
        publish_copy_at(
            from,
            stage,
            &name_utf16(OsStr::new("payload"))?,
            size,
            false,
            verify,
        )
    })
}

pub(crate) fn publish_tree_install(
    to: &Path,
    id: &str,
    entries: &[crate::models::InventoryEntry],
    object: impl Fn(&str) -> io::Result<std::path::PathBuf>,
) -> io::Result<()> {
    publish_install(to, id, |stage| {
        let payload = directory_child(
            stage,
            OsStr::new("payload"),
            true,
            DIRECTORY_ACCESS | GENERIC_WRITE,
        )?;
        write_tree(&payload, entries, object)
    })
}

fn publish_install(
    to: &Path,
    id: &str,
    write_payload: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    if to.file_name() != Some(OsStr::new(id)) {
        return Err(invalid("invalid installation identity"));
    }
    let chain = directory_chain(
        to.parent()
            .ok_or_else(|| invalid("installation has no parent"))?,
        false,
    )?;
    let parent = chain.last().unwrap();
    require_case_insensitive(parent)?;
    let temporary_name = format!(".app-depot-install-{}", uuid::Uuid::new_v4());
    let stage = directory_child(
        parent,
        OsStr::new(&temporary_name),
        true,
        DIRECTORY_ACCESS | DELETE | GENERIC_WRITE,
    )?;
    write_payload(&stage)?;
    let mut marker = open_child(
        &stage,
        OsStr::new(".app-depot-installation-id"),
        GENERIC_WRITE | SYNCHRONIZE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        0,
    )?;
    marker.write_all(id.as_bytes())?;
    marker.sync_all()?;
    drop(marker);
    stage.sync_all()?;
    #[cfg(test)]
    pause_tree_test("before-tree-install-publication");
    rename_noreplace(&stage, parent, &name_utf16(OsStr::new(id))?)?;
    sync_directory(to.parent().unwrap())
}

pub(crate) fn inspect_installed_payload(to: &Path, id: &str) -> io::Result<File> {
    let chain = installation_chain(to, id)?;
    open_installed_file(chain.last().unwrap(), "payload")
}

pub(crate) fn verify_tree_install(
    to: &Path,
    id: &str,
    entries: &[crate::models::InventoryEntry],
) -> io::Result<()> {
    let _installation = installation_chain(to, id)?;
    verify_tree(&to.join("payload"), entries).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            invalid("installed Tree is incomplete")
        } else {
            error
        }
    })
}

/// Keep the installation ancestors, working directory and candidate image pinned.
pub(crate) struct LaunchBinding {
    _installation: Vec<File>,
    _working_directory: Vec<File>,
    _image_directories: Vec<File>,
    _image: File,
}

impl LaunchBinding {
    pub(crate) fn matches(&self, other: &Self) -> io::Result<bool> {
        let work = |binding: &Self| {
            let file = binding
                ._working_directory
                .last()
                .or_else(|| binding._installation.last())
                .unwrap();
            file_identity(file)
        };
        Ok(
            file_identity(&self._image)? == file_identity(&other._image)?
                && work(self)? == work(other)?,
        )
    }
}

pub(crate) fn bind_launch_target(
    installation: &Path,
    id: &str,
    target: &str,
    working_directory: &str,
) -> io::Result<LaunchBinding> {
    let installation = installation_chain(installation, id)?;
    let root = installation.last().unwrap();
    require_case_insensitive(root)?;
    let (parents, leaf) = target.rsplit_once('/').unwrap_or(("", target));
    let image_directories = launch_directories(root, parents)?;
    let working_directory = launch_directories(root, working_directory)?;
    let mut image = open_installed_file(image_directories.last().unwrap_or(root), leaf)?;
    require_single_link(&image)?;
    validate_amd64_image(&mut image)?;
    Ok(LaunchBinding {
        _installation: installation,
        _working_directory: working_directory,
        _image_directories: image_directories,
        _image: image,
    })
}

fn launch_directories(root: &File, relative: &str) -> io::Result<Vec<File>> {
    let mut chain = Vec::new();
    if !relative.is_empty() {
        for part in relative.split('/') {
            let current = chain.last().unwrap_or(root);
            require_case_insensitive(current)?;
            chain.push(directory_child(
                current,
                OsStr::new(part),
                false,
                DIRECTORY_ACCESS,
            )?);
        }
        require_case_insensitive(chain.last().unwrap())?;
    }
    Ok(chain)
}

fn validate_amd64_image(file: &mut File) -> io::Result<()> {
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos)?;
    if &dos[..2] != b"MZ" {
        return Err(invalid("launch target is not a Windows executable image"));
    }
    let offset = u64::from(u32::from_le_bytes(dos[60..64].try_into().unwrap()));
    if offset < 64 || offset + 26 > file.metadata()?.len() {
        return Err(invalid("invalid executable header offset"));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut pe = [0u8; 26];
    file.read_exact(&mut pe)?;
    let machine = u16::from_le_bytes([pe[4], pe[5]]);
    let characteristics = u16::from_le_bytes([pe[22], pe[23]]);
    let optional_size = u16::from_le_bytes([pe[20], pe[21]]);
    if &pe[..4] != b"PE\0\0"
        || machine != 0x8664
        || characteristics & 0x2002 != 0x0002
        || optional_size < 2
        || pe[24..26] != [0x0b, 0x02]
    {
        return Err(invalid(
            "launch target is not a native Windows x64 executable",
        ));
    }
    Ok(())
}

fn installation_chain(to: &Path, id: &str) -> io::Result<Vec<File>> {
    let chain = directory_chain(to, false)?;
    let directory = chain.last().unwrap();
    let mut marker = String::new();
    open_installed_file(directory, ".app-depot-installation-id")?
        .take(37)
        .read_to_string(&mut marker)?;
    if marker != id {
        return Err(invalid("installation ownership marker mismatch"));
    }
    Ok(chain)
}

fn open_installed_file(directory: &File, name: &str) -> io::Result<File> {
    let file = open_child(
        directory,
        OsStr::new(name),
        GENERIC_READ | SYNCHRONIZE,
        FILE_OPEN,
        FILE_OPEN_REPARSE_POINT,
        FILE_SHARE_READ,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            invalid("existing installation lacks marker or payload")
        } else {
            error
        }
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid("unsafe file in installation"));
    }
    Ok(file)
}

/// Read-only removal preflight. A successful scan does not authorize deletion.
/// A future transaction must revalidate ownership while holding deletion handles.
pub(crate) fn inspect_removal_layout(
    installation: &Path,
    id: &str,
    verify_file: impl FnOnce(&File) -> io::Result<()>,
    entries: Option<&[crate::models::InventoryEntry]>,
) -> io::Result<()> {
    let chain = directory_chain(installation, false)?;
    let root = chain.last().unwrap();
    require_case_insensitive(root)?;
    for entry in std::fs::read_dir(installation)? {
        let name = entry?.file_name();
        if name != ".app-depot-installation-id" && name != "payload" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected installation entry: {}", name.to_string_lossy()),
            ));
        }
    }
    let marker = open_removal_file(root, OsStr::new(".app-depot-installation-id"))?;
    let mut contents = String::new();
    (&marker).take(37).read_to_string(&mut contents)?;
    if contents != id {
        return Err(invalid("installation ownership marker mismatch"));
    }
    match entries {
        Some(entries) => verify_tree_inner(&installation.join("payload"), entries, true),
        None => verify_file(&open_removal_file(root, OsStr::new("payload"))?),
    }
}

fn open_removal_file(parent: &File, name: &OsStr) -> io::Result<File> {
    let file = open_child(
        parent,
        name,
        GENERIC_READ | DELETE | SYNCHRONIZE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
        0,
    )?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid("reparse point in managed installation"));
    }
    require_single_link(&file)?;
    Ok(file)
}

/// Constructs a new tree under an exclusive directory handle and publishes it by that handle.
pub(crate) fn publish_tree(
    destination: &Path,
    entries: &[crate::models::InventoryEntry],
    object: impl Fn(&str) -> io::Result<std::path::PathBuf>,
) -> io::Result<()> {
    let parent_path = destination
        .parent()
        .ok_or_else(|| invalid("Tree has no parent"))?;
    let chain = directory_chain(parent_path, false)?;
    let parent = chain.last().unwrap();
    require_case_insensitive(parent)?;
    let name = destination
        .file_name()
        .ok_or_else(|| invalid("Tree has no name"))?;
    let stage_name = format!(".app-depot-tree-{}", uuid::Uuid::new_v4());
    let stage = directory_child(
        parent,
        OsStr::new(&stage_name),
        true,
        DIRECTORY_ACCESS | DELETE | GENERIC_WRITE,
    )?;
    write_tree(&stage, entries, object)?;
    rename_noreplace(&stage, parent, &name_utf16(name)?)?;
    sync_directory(parent_path)
}

fn write_tree(
    stage: &File,
    entries: &[crate::models::InventoryEntry],
    object: impl Fn(&str) -> io::Result<std::path::PathBuf>,
) -> io::Result<()> {
    use crate::models::InventoryEntry;
    for entry in entries {
        let mut components = entry.path().rsplitn(2, '/');
        let leaf = OsStr::new(components.next().unwrap());
        let mut ancestors = Vec::new();
        for part in components
            .next()
            .into_iter()
            .flat_map(|prefix| prefix.split('/'))
        {
            let ancestor = directory_child(
                ancestors.last().unwrap_or(stage),
                OsStr::new(part),
                false,
                DIRECTORY_ACCESS | GENERIC_WRITE,
            )?;
            ancestors.push(ancestor);
        }
        let target = ancestors.last().unwrap_or(stage);
        require_case_insensitive(target)?;
        match entry {
            InventoryEntry::Directory { .. } => {
                directory_child(target, leaf, true, DIRECTORY_ACCESS | GENERIC_WRITE)?
                    .sync_all()?;
            }
            InventoryEntry::File { size, sha256, .. } => {
                publish_copy_at(
                    &object(sha256)?,
                    target,
                    &name_utf16(leaf)?,
                    *size,
                    false,
                    |file| {
                        let actual = crate::transfer::hash_open_file(file, *size)
                            .map_err(io::Error::other)?;
                        if actual != *sha256 {
                            return Err(invalid("Tree source hash mismatch"));
                        }
                        Ok(())
                    },
                )?;
            }
        }
        target.sync_all()?;
        #[cfg(test)]
        pause_tree_test("partial-tree-materialization");
    }
    stage.sync_all()
}

/// The open directory chain pins each enumerated path. Entries are reopened relative to
/// pinned handles and checked for reparse points before reading or descending.
pub(crate) fn verify_tree(
    path: &Path,
    entries: &[crate::models::InventoryEntry],
) -> io::Result<()> {
    verify_tree_inner(path, entries, false)
}

fn verify_tree_inner(
    path: &Path,
    entries: &[crate::models::InventoryEntry],
    removal: bool,
) -> io::Result<()> {
    use crate::models::InventoryEntry;
    use std::collections::HashMap;
    fn scan(
        dir: &File,
        path: &Path,
        prefix: &str,
        remaining: &mut HashMap<&str, &InventoryEntry>,
        removal: bool,
    ) -> io::Result<()> {
        require_case_insensitive(dir)?;
        for child in std::fs::read_dir(path)? {
            let child = child?;
            let name = child.file_name();
            let component = name
                .to_str()
                .ok_or_else(|| invalid("Tree name is not Unicode"))?;
            validate_component(component)?;
            let relative = if prefix.is_empty() {
                component.to_owned()
            } else {
                format!("{prefix}/{component}")
            };
            let entry = remaining.remove(relative.as_str()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected Tree entry: {relative}"),
                )
            })?;
            match entry {
                InventoryEntry::Directory { .. } => {
                    let subdir = directory_child(dir, &name, false, DIRECTORY_ACCESS)?;
                    scan(&subdir, &child.path(), &relative, remaining, removal)?;
                }
                InventoryEntry::File { size, sha256, .. } => {
                    let file = if removal {
                        open_removal_file(dir, &name)?
                    } else {
                        open_child(
                            dir,
                            &name,
                            GENERIC_READ | SYNCHRONIZE,
                            FILE_OPEN,
                            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
                            FILE_SHARE_READ,
                        )?
                    };
                    let meta = file.metadata()?;
                    if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    {
                        return Err(invalid("unsafe Tree file"));
                    }
                    if crate::transfer::hash_open_file(&file, *size).map_err(io::Error::other)?
                        != *sha256
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Tree hash mismatch: {relative}"),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    let chain = directory_chain(path, false)?;
    let mut remaining = entries.iter().map(|entry| (entry.path(), entry)).collect();
    scan(chain.last().unwrap(), path, "", &mut remaining, removal)?;
    if let Some(missing) = remaining.keys().min() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("missing Tree entry: {missing}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn pause_tree_test(boundary: &str) {
    if let Some(stage) = std::env::var_os("APP_DEPOT_TREE_TEST_STAGE") {
        crate::transfer::pause_file_commit_test(Path::new(&stage), boundary);
    }
}

fn file_identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: FileIdInfo writes to a FILE_ID_INFO buffer and file stays open during the call.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

fn volume(file: &File) -> io::Result<u64> {
    Ok(file_identity(file)?.0)
}

fn delete_open_file(file: File) -> io::Result<()> {
    let mut status_block = IO_STATUS_BLOCK::default();
    let info = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE
            | FILE_DISPOSITION_POSIX_SEMANTICS
            | FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: the exclusive creation handle grants DELETE on this exact file. This call
    // keeps the handle and disposition buffer valid until synchronous completion.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status_block,
            (&raw const info).cast(),
            size_of::<FILE_DISPOSITION_INFORMATION_EX>() as u32,
            FileDispositionInformationEx,
        )
    };
    nt_result(status)
}

fn rename_noreplace(source: &File, target: &File, name: &[u16]) -> io::Result<()> {
    let mut info = RenameInformation {
        flags: 0,
        root: target.as_raw_handle(),
        name_bytes: std::mem::size_of_val(name) as u32,
        name: [0; MAX_NAME],
    };
    info.name[..name.len()].copy_from_slice(name);
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: fixed C layout checked against the SDK binding. Both endpoint handles and the
    // complete name buffer remain valid for this synchronous operation. Flags forbid replacement.
    let status = unsafe {
        NtSetInformationFile(
            source.as_raw_handle(),
            &mut status_block,
            (&raw const info).cast(),
            size_of::<RenameInformation>() as u32,
            FileRenameInformation,
        )
    };
    nt_result(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn failed_publication_cleans_only_its_exclusive_temporary_file() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let source = root.join("source");
        let target = root.join("target");
        let unrelated = root.join("unrelated");
        fs::write(&source, b"data").unwrap();
        fs::write(&unrelated, b"keep").unwrap();

        let error = publish_verified_copy(&source, &target, 4, |_| {
            let temporary = root
                .read_dir()
                .unwrap()
                .flatten()
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".app-depot-publish-")
                })
                .unwrap()
                .path();
            assert!(fs::rename(&temporary, root.join("substitute")).is_err());
            assert!(fs::rename(&unrelated, &temporary).is_err());
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture rejection",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
        assert_eq!(fs::read(&source).unwrap(), b"data");
        assert!(!target.exists());
        assert_eq!(root.read_dir().unwrap().count(), 2);
    }

    #[test]
    fn rejects_win32_path_aliases_and_streams() {
        for name in [
            "",
            ".",
            "..",
            "C:relative",
            r"\absolute",
            "x/y",
            "name:stream",
            "trailing.",
            "trailing ",
            "AUX.txt",
            "con .exe",
            "COM¹.txt",
            "Lpt²",
            "LPT³.exe",
            "CONOUT$",
        ] {
            assert!(validate_component(name).is_err(), "{name}");
        }
        assert!(validate_component("openxray.exe").is_ok());
        assert!(validate_component(&"😀".repeat(128)).is_err());
    }

    #[test]
    fn publishes_directory_once_and_preserves_both_trees_on_collision() {
        let fixture = tempfile::tempdir().unwrap();
        let from = fixture.path().join("staging");
        let to = fixture.path().join("installed");
        let colliding_destination = fixture.path().join("INSTALLED");
        ensure_directory(&from.join("empty")).unwrap();
        ensure_directory(&to).unwrap();
        fs::write(from.join("verified"), "new").unwrap();
        fs::write(to.join("existing"), "old").unwrap();
        assert!(rename_directory_noreplace(&from, &colliding_destination).is_err());
        assert_eq!(fs::read(to.join("existing")).unwrap(), b"old");
        assert_eq!(fs::read(from.join("verified")).unwrap(), b"new");
        fs::rename(&to, fixture.path().join("old")).unwrap();
        let pinned = directory_chain(&from, false).unwrap();
        assert!(rename_directory_noreplace(&from, &to).is_err());
        assert!(!to.exists());
        drop(pinned);
        // Retry after the conflict is independently resolved. Staging remains authoritative.
        rename_directory_noreplace(&from, &to).unwrap();
        assert!(!from.exists());
        assert!(to.join("empty").is_dir());
        assert_eq!(fs::read(to.join("verified")).unwrap(), b"new");
    }

    #[test]
    fn creates_regular_files_exclusively_and_reads_flushed_bytes() {
        use std::io::{Read, Write};
        let fixture = tempfile::tempdir().unwrap();
        let stage = fixture.path().join("staging");
        let file = stage.join("Object");
        ensure_directory(&stage).unwrap();
        let mut output = regular_file(&file, true).unwrap();
        output.write_all(b"verified bytes").unwrap();
        output.sync_all().unwrap();
        drop(output);
        assert_eq!(
            regular_file(&stage.join("object"), true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        let mut bytes = Vec::new();
        regular_file(&file, false)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"verified bytes");
    }
    #[test]
    fn launch_binding_pins_native_image_and_working_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let id = "6aba9be7-1f56-45c3-bdb6-df916d9235a9";
        let install = fixture.path().join(id);
        let working = install.join("payload/bin");
        std::fs::create_dir_all(&working).unwrap();
        std::fs::write(install.join(".app-depot-installation-id"), id).unwrap();
        let exe = working.join("fixture.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let binding =
            bind_launch_target(&install, id, "payload/bin/fixture.exe", "payload/bin").unwrap();
        assert!(std::fs::rename(&working, install.join("moved")).is_err());
        assert!(std::fs::OpenOptions::new().write(true).open(&exe).is_err());
        assert!(std::fs::rename(&exe, working.join("renamed.exe")).is_err());
        drop(binding);
        assert!(
            bind_launch_target(&install, id, "payload/../payload/bin/fixture.exe", "").is_err()
        );
        assert!(
            bind_launch_target(
                &install,
                id,
                "payload/bin/fixture.exe",
                "payload/../payload"
            )
            .is_err()
        );
        assert!(bind_launch_target(&install, id, "payload/bin/fixture.exe:alternate", "").is_err());
        assert!(bind_launch_target(&install, id, "payload/bin/missing.exe", "").is_err());
        let alias = working.join("alias.exe");
        std::fs::hard_link(&exe, &alias).unwrap();
        assert!(bind_launch_target(&install, id, "payload/bin/fixture.exe", "").is_err());
        std::fs::remove_file(alias).unwrap();
        let corrupt = working.join("corrupt.exe");
        std::fs::write(&corrupt, [0; 128]).unwrap();
        assert!(bind_launch_target(&install, id, "payload/bin/corrupt.exe", "").is_err());
    }
    #[test]
    fn launch_binding_refuses_junction_in_image_or_working_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let id = "35fd693c-20cf-4cb6-bf63-309ecfee41cb";
        let install = fixture.path().join(id);
        let payload = install.join("payload");
        let outside = fixture.path().join("outside");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(install.join(".app-depot-installation-id"), id).unwrap();
        std::fs::copy(
            std::env::current_exe().unwrap(),
            outside.join("fixture.exe"),
        )
        .unwrap();
        std::fs::copy(
            std::env::current_exe().unwrap(),
            payload.join("fixture.exe"),
        )
        .unwrap();

        // mklink is used only to manufacture a junction inside this test's own directory.
        let junction = payload.join("escape");
        use std::os::windows::process::CommandExt;
        let junction_name = junction.to_str().unwrap().trim_start_matches(r"\\?\");
        let outside_name = outside.to_str().unwrap().trim_start_matches(r"\\?\");
        let command = format!("mklink /J \"{junction_name}\" \"{outside_name}\"");
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c"])
            .raw_arg(&command)
            .output()
            .unwrap();
        assert!(output.status.success(), "{command}: {output:?}");
        assert!(bind_launch_target(&install, id, "payload/escape/fixture.exe", "payload").is_err());
        assert!(bind_launch_target(&install, id, "payload/fixture.exe", "payload/escape").is_err());
        std::fs::remove_dir(junction).unwrap();
        assert!(outside.join("fixture.exe").is_file());
    }

    #[test]
    fn pinned_fixture_can_start_with_native_process_creation() {
        const WORK: &str = "APP_DEPOT_TEST_PINNED_LAUNCH_DIR";
        if let Some(path) = std::env::var_os(WORK) {
            assert_eq!(
                std::env::current_dir().unwrap(),
                std::path::PathBuf::from(path)
            );
            assert!(std::env::var_os("APP_DEPOT_TEST_PINNED_LAUNCH_SECRET").is_none());
            std::fs::write("started", b"test child").unwrap();
            std::process::exit(23);
        }
        let fixture = tempfile::tempdir().unwrap();
        let id = "0a76b1dd-bf20-4ae9-8887-7c4dcb707f10";
        let install = fixture.path().join(id);
        let working = install.join("payload");
        std::fs::create_dir_all(&working).unwrap();
        std::fs::write(install.join(".app-depot-installation-id"), id).unwrap();
        let exe = working.join("fixture with spaces.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let binding =
            bind_launch_target(&install, id, "payload/fixture with spaces.exe", "payload").unwrap();
        let status = std::process::Command::new(&exe)
            .current_dir(&working)
            .env(WORK, &working)
            .env_remove("APP_DEPOT_TEST_PINNED_LAUNCH_SECRET")
            .args([
                "--exact",
                "windows_fs::tests::pinned_fixture_can_start_with_native_process_creation",
            ])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(23));
        assert_eq!(
            std::fs::read(working.join("started")).unwrap(),
            b"test child"
        );
        drop(binding);
    }
}
