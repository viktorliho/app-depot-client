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

// Resolve handle names only on the error path. Operations themselves stay handle-relative.
fn diagnostic_path(file: &File) -> std::path::PathBuf {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut buffer = vec![0u16; 32768];
    // SAFETY: the handle is live and buffer has the advertised capacity.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        return "<unresolved handle>".into();
    }
    std::ffi::OsString::from_wide(&buffer[..length]).into()
}

// Retry only the failed kernel operation, keeping the original parent/source pinned.
pub(crate) fn retry_sharing<T>(
    mut operation: impl FnMut() -> io::Result<T>,
    context: impl FnOnce() -> String,
) -> io::Result<T> {
    let mut delays = [25, 75, 200, 500, 1000].into_iter();
    loop {
        match operation() {
            Err(error)
                if error.raw_os_error()
                    == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32) =>
            {
                if let Some(ms) = delays.next() {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                } else {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("{}: {error}", context()),
                    ));
                }
            }
            result => return result,
        }
    }
}

fn open_child(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    share: u32,
) -> io::Result<File> {
    let component = name;
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
    retry_sharing(
        || {
            nt_result(unsafe {
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
            })
        },
        || {
            format!(
                "NtCreateFile {} access={access:#x} share={share:#x} disposition={disposition:#x}",
                diagnostic_path(parent).join(component).display()
            )
        },
    )?;
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
    let root = retry_sharing(
        || {
            OpenOptions::new()
                .access_mode(DIRECTORY_ACCESS)
                .share_mode(SHARE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&root)
        },
        || format!("CreateFileW {root} access={DIRECTORY_ACCESS:#x} share={SHARE:#x}"),
    )?;
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
    let directory = directory_child(
        chain.last().unwrap(),
        name,
        false,
        DIRECTORY_ACCESS | GENERIC_WRITE,
    )?;
    sync_handle(&directory)
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
    rename_noreplace(&source, Some(target), &name_utf16(to_name)?)
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
    publish_copy_at(from, target, &name, expected_size, true, |source, out| {
        copy_then_verify(source, out, expected_size, verify)
    })
}

fn copy_then_verify(
    source: File,
    out: &mut File,
    size: u64,
    verify: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    if io::copy(&mut source.take(size), out)? != size {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "publication source shortened",
        ));
    }
    out.seek(SeekFrom::Start(0))?;
    verify(out)
}

fn publish_copy_at(
    from: &Path,
    target: &File,
    name: &[u16],
    expected_size: u64,
    readonly: bool,
    copy: impl FnOnce(File, &mut File) -> io::Result<()>,
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
        copy(source, &mut temporary)?;
        if readonly {
            let mut permissions = temporary.metadata()?.permissions();
            permissions.set_readonly(true);
            temporary.set_permissions(permissions)?;
        }
        sync_handle(&temporary)?;
        rename_noreplace(&temporary, None, name)
    })();
    if let Err(original) = &result {
        delete_open_file(temporary).map_err(|cleanup| {
            io::Error::new(
                original.kind(),
                format!("{original}; deleting temporary {temporary_name}: {cleanup}"),
            )
        })?;
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
            |source, out| copy_then_verify(source, out, size, verify),
        )
    })
}

/// Materialize a final installation layout from verified content objects.
/// Entry paths are destinations relative to payload, independent of the source
/// Artifact paths. The caller authorizes source references; this backend checks
/// size and digest again while copying, never linking to the object store.
/// The layout may reuse a source file ID or digest at multiple destinations.
/// The enclosing installation marker and no-replace publication are shared
/// with ordinary File/Tree installations.
pub(crate) fn publish_tree_install(
    to: &Path,
    id: &str,
    entries: &[crate::models::InventoryEntry],
    object: impl Fn(&str) -> io::Result<std::path::PathBuf>,
) -> io::Result<()> {
    stage_install_tree(to, id, entries, object, true, None, &mut |_, _| {})
}

/// Private attempt workspace. The complete published layout is checked exactly
/// once before the caller can hand any path to an installer.
pub(crate) fn prepare_verified_tree(
    to: &Path,
    id: &str,
    entries: &[crate::models::InventoryEntry],
    object: impl Fn(&str) -> io::Result<std::path::PathBuf>,
    progress: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let mut last = std::time::Instant::now();
    stage_install_tree(to, id, entries, object, false, None, &mut |done, total| {
        if done == total || last.elapsed() >= std::time::Duration::from_millis(500) {
            progress(&format!("Copying source entries: {done} / {total}"));
            last = std::time::Instant::now();
        }
    })?;
    progress("Verifying the complete prepared source");
    let _installation = installation_chain(to, id)?;
    verify_tree_inner(&to.join("payload"), entries, None, None, &mut |remaining| {
        if remaining == 0 || last.elapsed() >= std::time::Duration::from_millis(500) {
            progress(&format!(
                "Verifying source entries: {} / {}",
                entries.len() - remaining,
                entries.len()
            ));
            last = std::time::Instant::now();
        }
    })
}

pub(crate) type MemberRepair<'a> = &'a dyn Fn(&str, u64) -> io::Result<bool>;

pub(crate) fn stage_install_tree(
    to: &Path,
    id: &str,
    entries: &[crate::models::InventoryEntry],
    object: impl Fn(&str) -> io::Result<std::path::PathBuf>,
    verify_member: bool,
    repair: Option<MemberRepair<'_>>,
    progress: &mut dyn FnMut(usize, usize),
) -> io::Result<()> {
    validate_install_layout(entries)?;
    publish_install(to, id, |stage| {
        let payload = directory_child(
            stage,
            OsStr::new("payload"),
            true,
            DIRECTORY_ACCESS | GENERIC_WRITE,
        )?;
        write_tree(&payload, entries, object, verify_member, repair, progress)
    })
}

fn validate_install_layout(entries: &[crate::models::InventoryEntry]) -> io::Result<()> {
    use crate::models::InventoryEntry;
    use std::collections::HashSet;

    crate::tree::validate_destination(entries).map_err(io::Error::other)?;
    let directories: HashSet<_> = entries
        .iter()
        .filter_map(|entry| match entry {
            InventoryEntry::Directory { path } => Some(path.as_str()),
            InventoryEntry::File { .. } => None,
        })
        .collect();
    for entry in entries {
        if entry
            .path()
            .rsplit_once('/')
            .is_some_and(|(parent, _)| !directories.contains(parent))
        {
            return Err(invalid("installation layout parent directory is missing"));
        }
        if let InventoryEntry::File { sha256, .. } = entry
            && (sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        {
            return Err(invalid("invalid installation source digest"));
        }
    }
    Ok(())
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
    sync_handle(&marker)?;
    drop(marker);
    sync_handle(&stage)?;
    #[cfg(test)]
    pause_tree_test("before-tree-install-publication");
    rename_noreplace(&stage, None, &name_utf16(OsStr::new(id))?)?;
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
    verify_tree(&to.join("payload"), entries).map_err(incomplete_installation)
}

pub(crate) fn check_tree_install(
    to: &Path,
    id: &str,
    entries: &[crate::models::InventoryEntry],
    checks: &mut crate::materialization::LayoutChecks,
) -> io::Result<()> {
    let _installation = installation_chain(to, id)?;
    verify_tree_inner(
        &to.join("payload"),
        entries,
        None,
        Some(checks),
        &mut |_| {},
    )
    .map_err(incomplete_installation)
}

fn incomplete_installation(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::NotFound {
        invalid("installed Tree is incomplete")
    } else {
        error
    }
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
    bind_image(
        installation,
        id,
        target,
        working_directory,
        cfg!(target_arch = "x86_64"),
    )
}

pub(crate) fn bind_image(
    installation: &Path,
    id: &str,
    target: &str,
    working_directory: &str,
    allow_x86: bool,
) -> io::Result<LaunchBinding> {
    let installation = installation_chain(installation, id)?;
    let root = installation.last().unwrap();
    require_case_insensitive(root)?;
    let (parents, leaf) = target.rsplit_once('/').unwrap_or(("", target));
    let image_directories = launch_directories(root, parents)?;
    let working_directory = launch_directories(root, working_directory)?;
    let mut image = open_installed_file(image_directories.last().unwrap_or(root), leaf)?;
    require_single_link(&image)?;
    validate_image(&mut image, allow_x86)?;
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

fn validate_image(file: &mut File, allow_x86: bool) -> io::Result<()> {
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
        || !((machine == 0x8664 && pe[24..26] == [0x0b, 0x02])
            || (allow_x86 && machine == 0x14c && pe[24..26] == [0x0b, 0x01]))
        || characteristics & 0x2002 != 0x0002
        || optional_size < 2
    {
        return Err(invalid(
            "launch target is not a compatible native Windows executable",
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

pub(crate) type RemovalIdentities = std::collections::BTreeMap<String, (u64, [u8; 16])>;

/// Validate the entire surviving quarantine before deleting any entry.
pub(crate) fn cleanup_removal(path: &Path, expected: &RemovalIdentities) -> io::Result<()> {
    fn scan(
        path: &Path,
        relative: &str,
        directory: File,
        expected: &RemovalIdentities,
        handles: &mut Vec<File>,
    ) -> io::Result<()> {
        check_identity(&directory, relative, expected)?;
        require_case_insensitive(&directory)?;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name();
            let component = name
                .to_str()
                .ok_or_else(|| invalid("non-Unicode quarantine name"))?;
            let key = if relative == "." {
                component.to_owned()
            } else {
                format!("{relative}/{component}")
            };
            if !expected.contains_key(&key) {
                return Err(invalid("unexpected quarantine entry; contents retained"));
            }
            if entry.file_type()?.is_dir() {
                let child = open_removal_directory(&directory, &name)?;
                scan(&entry.path(), &key, child, expected, handles)?;
            } else {
                let child = open_removal_file(&directory, &name)?;
                check_identity(&child, &key, expected)?;
                handles.push(child);
            }
        }
        handles.push(directory);
        Ok(())
    }

    fn check_identity(file: &File, key: &str, expected: &RemovalIdentities) -> io::Result<()> {
        if expected.get(key) != Some(&file_identity(file)?) {
            return Err(invalid("quarantine object identity mismatch"));
        }
        Ok(())
    }

    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing quarantine parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("missing quarantine name"))?;
    let chain = directory_chain(parent, false)?;
    let directory = chain.last().unwrap();
    if expected.get("..") != Some(&file_identity(directory)?) {
        return Err(invalid("quarantine parent identity mismatch"));
    }
    let root = match open_removal_directory(directory, name) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut handles = Vec::new();
    scan(path, ".", root, expected, &mut handles)?;
    for file in handles {
        delete_open_file(file)?;
        #[cfg(test)]
        if std::env::var_os("APP_DEPOT_CLEANUP_CRASH").is_some() {
            let signal = parent.join(".removal-test-ready");
            File::create_new(signal)?;
            loop {
                std::thread::park();
            }
        }
    }
    Ok(())
}

fn reject_named_streams(file: &File) -> io::Result<()> {
    use windows_sys::Win32::{
        Foundation::ERROR_HANDLE_EOF,
        Storage::FileSystem::{FILE_STREAM_INFO, FileStreamInfo},
    };
    let mut buffer = [0u64; 128];
    // SAFETY: the buffer is writable, sized and 8-byte aligned, and the handle is live.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStreamInfo,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
        )
    };
    if ok == 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
            Ok(())
        } else {
            Err(error)
        };
    }
    // SAFETY: the initialized buffer is aligned and larger than a complete header.
    let info = unsafe { &*buffer.as_ptr().cast::<FILE_STREAM_INFO>() };
    let name = [58u16, 58, 36, 68, 65, 84, 65]; // ::$DATA
    let offset = offset_of!(FILE_STREAM_INFO, StreamName);
    // SAFETY: the fixed range is within the initialized and aligned buffer.
    let stream = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr().cast::<u8>().add(offset).cast::<u16>(),
            name.len(),
        )
    };
    if info.NextEntryOffset != 0
        || info.StreamNameLength as usize != size_of_val(&name)
        || stream != name
    {
        return Err(invalid("unexpected NTFS data stream; quarantine retained"));
    }
    Ok(())
}

pub(crate) struct RemovalInspection {
    chain: Vec<File>,
    root: File,
    pub identities: RemovalIdentities,
}

impl RemovalInspection {
    pub(crate) fn quarantine(&self, name: &OsStr) -> io::Result<()> {
        let parent = self.chain.last().unwrap();
        require_case_insensitive(parent)?;
        if volume(&self.root)? != volume(parent)? {
            return Err(invalid("quarantine must stay on the installation volume"));
        }
        rename_noreplace(&self.root, None, &name_utf16(name)?)
    }
}

/// Verify the layout and keep the verified directory pinned through quarantine.
pub(crate) fn inspect_removal_layout(
    installation: &Path,
    id: &str,
    verify_file: impl FnOnce(&File) -> io::Result<()>,
    entries: Option<&[crate::models::InventoryEntry]>,
) -> io::Result<RemovalInspection> {
    let parent = installation
        .parent()
        .ok_or_else(|| invalid("missing installation parent"))?;
    let name = installation
        .file_name()
        .ok_or_else(|| invalid("missing installation name"))?;
    let chain = directory_chain(parent, false)?;
    let root = open_removal_directory(chain.last().unwrap(), name)?;
    require_case_insensitive(&root)?;
    for entry in std::fs::read_dir(installation)? {
        let name = entry?.file_name();
        if name != ".app-depot-installation-id" && name != "payload" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected installation entry: {}", name.to_string_lossy()),
            ));
        }
    }
    let marker = open_removal_file(&root, OsStr::new(".app-depot-installation-id"))?;
    let mut contents = String::new();
    (&marker).take(37).read_to_string(&mut contents)?;
    if contents != id {
        return Err(invalid("installation ownership marker mismatch"));
    }
    let mut identities = RemovalIdentities::from([
        (".".into(), file_identity(&root)?),
        ("..".into(), file_identity(chain.last().unwrap())?),
        (".app-depot-installation-id".into(), file_identity(&marker)?),
    ]);
    match entries {
        Some(entries) => {
            let payload = open_removal_directory(&root, OsStr::new("payload"))?;
            identities.insert("payload".into(), file_identity(&payload)?);
            verify_tree_inner(
                &installation.join("payload"),
                entries,
                Some((&payload, &mut identities)),
                None,
                &mut |_| {},
            )?;
        }
        None => {
            let payload = open_removal_file(&root, OsStr::new("payload"))?;
            verify_file(&payload)?;
            identities.insert("payload".into(), file_identity(&payload)?);
        }
    }
    Ok(RemovalInspection {
        chain,
        root,
        identities,
    })
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
    reject_named_streams(&file)?;
    Ok(file)
}

// Recheck and move the same exclusive plain-file handle. Never overwrite a competing name.
pub(crate) fn quarantine_tree_object(
    from: &Path,
    backup: &Path,
    check: impl FnOnce(&File) -> io::Result<Option<String>>,
) -> io::Result<Option<String>> {
    let parents = directory_chain(from.parent().unwrap(), false)?;
    let target = directory_chain(backup.parent().unwrap(), false)?;
    let source = open_removal_file(parents.last().unwrap(), from.file_name().unwrap())?;
    let detail = check(&source)?;
    if let Some(reason) = &detail {
        rename_noreplace(
            &source,
            Some(target.last().unwrap()),
            &name_utf16(backup.file_name().unwrap())?,
        )
        .map_err(|error| io::Error::new(error.kind(), format!("{reason}; quarantine: {error}")))?;
    }
    Ok(detail)
}

fn open_removal_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    let dir = directory_child(parent, name, false, DIRECTORY_ACCESS | DELETE)?;
    reject_named_streams(&dir)?;
    Ok(dir)
}

/// Exclusive temporary source owner; all publication staging stays beneath it.
pub(crate) fn create_prerequisite_workspace(path: &Path, attempt: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing prerequisite workspace parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("missing prerequisite workspace name"))?;
    let chain = directory_chain(parent, false)?;
    let root = directory_child(
        chain.last().unwrap(),
        name,
        true,
        DIRECTORY_ACCESS | GENERIC_WRITE,
    )?;
    let mut marker = open_child(
        &root,
        OsStr::new(".app-depot-prerequisite-id"),
        GENERIC_WRITE | SYNCHRONIZE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        0,
    )?;
    marker.write_all(attempt.as_bytes())?;
    sync_handle(&marker)?;
    sync_handle(&root)?;
    sync_directory(parent)
}

/// Delete only known private staging names. No CAS, published tree or installation
/// paths are traversed. Preflight the whole selection before deleting by handle.
pub(crate) fn cleanup_job_stage(
    path: &Path,
    parts: &std::collections::HashSet<String>,
) -> io::Result<()> {
    let parent = path.parent().unwrap();
    let chain = directory_chain(parent, false)?;
    let root = match open_removal_directory(chain.last().unwrap(), path.file_name().unwrap()) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    require_case_insensitive(&root)?;
    let mut handles = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "files" {
            let directory = open_removal_directory(&root, &name)?;
            require_case_insensitive(&directory)?;
            for child in std::fs::read_dir(entry.path())? {
                let child = child?;
                if !child
                    .file_name()
                    .to_str()
                    .is_some_and(|n| parts.contains(n))
                {
                    return Err(io::Error::other(format!(
                        "unexpected staging entry {}",
                        child.path().display()
                    )));
                }
                handles.push(open_removal_file(&directory, &child.file_name())?);
            }
            handles.push(directory);
        } else if name == "file.part"
            || name == "job.lock"
            || name.to_str().is_some_and(|n| {
                n.strip_prefix("invalid-").is_some_and(|rest| {
                    rest.split_once('-').is_some_and(|(hash, id)| {
                        hash.len() == 64
                            && hash.bytes().all(|b| b.is_ascii_hexdigit())
                            && uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id)
                    })
                })
            })
        {
            handles.push(open_removal_file(&root, &name)?);
        } else {
            return Err(io::Error::other(format!(
                "unexpected staging entry {}",
                entry.path().display()
            )));
        }
    }
    for file in handles {
        delete_open_file(file)?;
    }
    delete_open_file(root)?;
    sync_directory(parent)
}

/// Ephemeral workspace cleanup, not removal of a user installation. Pin the
/// entire owned tree before deletion, refusing reparse points, extra streams
/// and multiply-linked files. Keep the ownership marker until the end.
pub(crate) fn cleanup_prerequisite_workspace(path: &Path, attempt: &str) -> io::Result<()> {
    fn scan(path: &Path, directory: &File, handles: &mut Vec<File>, root: bool) -> io::Result<()> {
        require_case_insensitive(directory)?;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let name = entry.file_name();
            if root && name == ".app-depot-prerequisite-id" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                let child = open_removal_directory(directory, &name)?;
                scan(&entry.path(), &child, handles, false)?;
                handles.push(child);
            } else {
                handles.push(open_removal_file(directory, &name)?);
            }
        }
        Ok(())
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing prerequisite workspace parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("missing prerequisite workspace name"))?;
    let chain = directory_chain(parent, false)?;
    let root = match open_removal_directory(chain.last().unwrap(), name) {
        Ok(root) => root,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let marker = open_removal_file(&root, OsStr::new(".app-depot-prerequisite-id"))?;
    let mut owner = String::new();
    (&marker).take(37).read_to_string(&mut owner)?;
    if owner != attempt {
        return Err(invalid("prerequisite workspace ownership marker mismatch"));
    }
    let mut handles = Vec::new();
    scan(path, &root, &mut handles, true)?;
    for file in handles {
        delete_open_file(file)?;
    }
    delete_open_file(marker)?;
    delete_open_file(root)?;
    sync_directory(parent)
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
    write_tree(&stage, entries, object, true, None, &mut |_, _| {})?;
    rename_noreplace(&stage, None, &name_utf16(name)?)?;
    sync_directory(parent_path)
}

fn write_tree(
    stage: &File,
    entries: &[crate::models::InventoryEntry],
    object: impl Fn(&str) -> io::Result<std::path::PathBuf>,
    verify_member: bool,
    repair: Option<MemberRepair<'_>>,
    progress: &mut dyn FnMut(usize, usize),
) -> io::Result<()> {
    use crate::models::InventoryEntry;
    // The tree itself is created exclusively and published by no-replace
    // rename. Private attempts verify their members together after publication.
    let mut ordered: Vec<_> = entries.iter().collect();
    ordered.sort_unstable_by(|left, right| left.path().cmp(right.path()));
    let mut current_parent = "";
    let mut ancestors = Vec::new();
    require_case_insensitive(stage)?;
    for (i, entry) in ordered.into_iter().enumerate() {
        let (parent, leaf) = entry.path().rsplit_once('/').unwrap_or(("", entry.path()));
        if parent != current_parent {
            sync_handle(ancestors.last().unwrap_or(stage))?;
            ancestors.clear();
            for part in parent.split('/').filter(|part| !part.is_empty()) {
                let dir = directory_child(
                    ancestors.last().unwrap_or(stage),
                    OsStr::new(part),
                    false,
                    DIRECTORY_ACCESS | GENERIC_WRITE,
                )?;
                require_case_insensitive(&dir)?;
                ancestors.push(dir);
            }
            current_parent = parent;
        }
        let target = ancestors.last().unwrap_or(stage);
        match entry {
            InventoryEntry::Directory { .. } => {
                let directory = directory_child(
                    target,
                    OsStr::new(leaf),
                    true,
                    DIRECTORY_ACCESS | GENERIC_WRITE,
                )?;
                sync_handle(&directory)?;
            }
            InventoryEntry::File { size, sha256, .. } => {
                let path = object(sha256)?;
                if verify_member {
                    let copy = || {
                        publish_copy_at(
                            &path,
                            target,
                            &name_utf16(OsStr::new(leaf))?,
                            *size,
                            false,
                            |source, out| {
                                crate::transfer::copy_verified(source, out, *size, sha256)
                            },
                        )
                    };
                    if let Err(error) = copy() {
                        if matches!(
                            error.kind(),
                            io::ErrorKind::InvalidData
                                | io::ErrorKind::NotFound
                                | io::ErrorKind::UnexpectedEof
                        ) && let Some(repair) = repair
                            && repair(sha256, *size)?
                        {
                            copy()?;
                        } else {
                            return Err(io::Error::new(
                                error.kind(),
                                format!(
                                    "prepare {} from {} (expected {size} bytes, SHA-256 {sha256}): {error}",
                                    entry.path(),
                                    path.display()
                                ),
                            ));
                        }
                    }
                } else {
                    copy_workspace_file(&path, target, OsStr::new(leaf), *size)?;
                }
            }
        }
        progress(i + 1, entries.len());
        #[cfg(test)]
        pause_tree_test("partial-tree-materialization");
    }
    if let Some(dir) = ancestors.last() {
        sync_handle(dir)?;
    }
    sync_handle(stage)
}

// The stage is exclusive. Private attempts verify the complete published tree.
fn copy_workspace_file(path: &Path, target: &File, leaf: &OsStr, size: u64) -> io::Result<()> {
    let source = open_regular(path, false, false, FILE_SHARE_READ)?;
    if volume(&source)? != volume(target)? || source.metadata()?.len() != size {
        return Err(invalid("Tree source volume or size mismatch"));
    }
    let mut out = open_child(
        target,
        leaf,
        GENERIC_WRITE | SYNCHRONIZE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        0,
    )?;
    if io::copy(&mut source.take(size), &mut out)? != size {
        return Err(invalid("Tree source shortened"));
    }
    sync_handle(&out)
}

/// The open directory chain pins each enumerated path. Entries are reopened relative to
/// pinned handles and checked for reparse points before reading or descending.
pub(crate) fn verify_tree(
    path: &Path,
    entries: &[crate::models::InventoryEntry],
) -> io::Result<()> {
    verify_tree_inner(path, entries, None, None, &mut |_| {})
}

fn verify_tree_inner(
    path: &Path,
    entries: &[crate::models::InventoryEntry],
    removal_root: Option<(&File, &mut RemovalIdentities)>,
    checks: Option<&mut crate::materialization::LayoutChecks>,
    report_remaining: &mut dyn FnMut(usize),
) -> io::Result<()> {
    use crate::models::InventoryEntry;
    use std::collections::HashMap;
    fn scan(
        dir: &File,
        path: &Path,
        prefix: &str,
        remaining: &mut HashMap<&str, &InventoryEntry>,
        mut identities: Option<&mut RemovalIdentities>,
        mut checks: Option<&mut crate::materialization::LayoutChecks>,
        report_remaining: &mut dyn FnMut(usize),
    ) -> io::Result<()> {
        require_case_insensitive(dir)?;
        for child in retry_sharing(
            || std::fs::read_dir(path),
            || format!("read_dir {}", path.display()),
        )? {
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
                    let subdir = if let Some(ids) = identities.as_deref_mut() {
                        let subdir = open_removal_directory(dir, &name)?;
                        ids.insert(format!("payload/{relative}"), file_identity(&subdir)?);
                        subdir
                    } else {
                        directory_child(dir, &name, false, DIRECTORY_ACCESS)?
                    };
                    scan(
                        &subdir,
                        &child.path(),
                        &relative,
                        remaining,
                        identities.as_deref_mut(),
                        checks.as_deref_mut(),
                        report_remaining,
                    )?;
                }
                InventoryEntry::File { size, sha256, .. } => {
                    let file = if identities.is_some() {
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
                    if let Some(cache) = checks.as_deref_mut() {
                        require_single_link(&file)?;
                        cache.check(&relative, &file, *size, sha256)?;
                    } else if crate::transfer::hash_open_file(&file, *size)
                        .map_err(io::Error::other)?
                        != *sha256
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Tree hash mismatch: {relative}"),
                        ));
                    }
                    if let Some(ids) = identities.as_deref_mut() {
                        ids.insert(format!("payload/{relative}"), file_identity(&file)?);
                    }
                }
            }
            report_remaining(remaining.len());
        }
        Ok(())
    }
    let mut remaining = entries.iter().map(|entry| (entry.path(), entry)).collect();
    if let Some((root, ids)) = removal_root {
        scan(
            root,
            path,
            "",
            &mut remaining,
            Some(ids),
            checks,
            report_remaining,
        )?;
    } else {
        let chain = directory_chain(path, false)?;
        scan(
            chain.last().unwrap(),
            path,
            "",
            &mut remaining,
            None,
            checks,
            report_remaining,
        )?;
    }
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

pub(crate) fn installation_file_stamp(file: &File) -> io::Result<(u64, [u8; 16], i64, i64)> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_BASIC_INFO, FileBasicInfo};
    let mut info = FILE_BASIC_INFO::default();
    // SAFETY: the open handle and output buffer match FileBasicInfo.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&raw mut info).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let (volume, id) = file_identity(file)?;
    Ok((volume, id, info.ChangeTime, info.LastWriteTime))
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

fn sync_handle(file: &File) -> io::Result<()> {
    retry_sharing(
        || file.sync_all(),
        || format!("FlushFileBuffers {}", diagnostic_path(file).display()),
    )
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
    retry_sharing(
        || {
            nt_result(unsafe {
                NtSetInformationFile(
                    file.as_raw_handle(),
                    &mut status_block,
                    (&raw const info).cast(),
                    size_of::<FILE_DISPOSITION_INFORMATION_EX>() as u32,
                    FileDispositionInformationEx,
                )
            })
        },
        || {
            format!(
                "NtSetInformationFile(FileDispositionInformationEx) {}",
                diagnostic_path(&file).display()
            )
        },
    )
}

// A simple leaf with NULL RootDirectory renames in place. Supplying a root
// asks Windows to open the target directory again with write access.
fn rename_noreplace(source: &File, target: Option<&File>, name: &[u16]) -> io::Result<()> {
    let mut info = RenameInformation {
        flags: 0,
        root: target.map_or(std::ptr::null_mut(), AsRawHandle::as_raw_handle),
        name_bytes: std::mem::size_of_val(name) as u32,
        name: [0; MAX_NAME],
    };
    info.name[..name.len()].copy_from_slice(name);
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: fixed C layout checked against the SDK binding. Both endpoint handles and the
    // complete name buffer remain valid for this synchronous operation. Flags forbid replacement.
    retry_sharing(
        || {
            nt_result(unsafe {
                NtSetInformationFile(
                    source.as_raw_handle(),
                    &mut status_block,
                    (&raw const info).cast(),
                    size_of::<RenameInformation>() as u32,
                    FileRenameInformation,
                )
            })
        },
        || {
            let from = diagnostic_path(source);
            let leaf = String::from_utf16_lossy(name);
            let to = match target {
                Some(directory) => diagnostic_path(directory).join(leaf),
                None => from.with_file_name(leaf),
            };
            format!(
                "NtSetInformationFile(FileRenameInformation) {} -> {}",
                from.display(),
                to.display()
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::InventoryEntry;
    use sha2::{Digest, Sha256};
    use std::fs;

    #[test]
    fn verified_prerequisite_tree_rejects_corrupt_supporting_file() {
        let fixture = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let dst = fixture.path().join(&id);
        let object = fixture.path().join("object");
        fs::write(&object, b"good").unwrap();
        let entries = [layout_dir("nested"), layout_file("nested/support", b"good")];
        let error =
            prepare_verified_tree(&dst, &id, &entries, |_| Ok(object.clone()), &mut |phase| {
                if phase == "Verifying the complete prepared source" {
                    fs::write(dst.join("payload/nested/support"), b"evil").unwrap();
                }
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(object).unwrap(), b"good");
    }

    fn layout_file(path: &str, bytes: &[u8]) -> InventoryEntry {
        InventoryEntry::File {
            path: path.into(),
            file_id: "same-source-file-id".into(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            executable: false,
        }
    }

    fn layout_dir(path: &str) -> InventoryEntry {
        InventoryEntry::Directory { path: path.into() }
    }

    #[test]
    fn manifest_destination_layout_uses_independent_cas_copies() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let objects = root.join("objects");
        ensure_directory(&objects).unwrap();
        for bytes in [b"base".as_slice(), b"overlay".as_slice()] {
            fs::write(objects.join(format!("{:x}", Sha256::digest(bytes))), bytes).unwrap();
        }
        use crate::installation::{Plan, ResolveRequest};
        use serde_json::json;
        let source = |bytes: &[u8], association: uuid::Uuid| {
            let hash = format!("{:x}", Sha256::digest(bytes));
            json!({
                "associationId": association.to_string(), "artifactId": format!("adfile:sha256:{hash}"),
                "fileId": null, "sha256": hash, "size": bytes.len(), "executable": false
            })
        };
        let base = source(b"base", uuid::Uuid::new_v4());
        let overlay = source(b"overlay", uuid::Uuid::new_v4());
        let release = uuid::Uuid::new_v4().to_string();
        let revision = uuid::Uuid::new_v4().to_string();
        let request = ResolveRequest {
            platform: "windows/x86_64".into(),
            revision_id: Some(revision.clone()),
            selections: Default::default(),
        };
        let plan = Plan::parse(
            &json!({
                "version": 1, "appId": uuid::Uuid::new_v4().to_string(),
                "releaseId": release, "revisionId": revision, "platform": request.platform,
                "selections": {}, "unresolved": [],
                "files": {"bin/renamed.dat": base, "localization/text.dat": overlay, "copy.dat": base},
                "directories": ["bin", "empty", "empty/nested", "localization"],
                "prerequisites": [], "launch": null
            }),
            &release,
            &request,
        ).unwrap();
        let mut entries = crate::materialization::destination_layout(&plan).unwrap();
        // Also exercise backend ordering independently of the shared projection.
        entries.reverse();
        let id = uuid::Uuid::new_v4().to_string();
        let target = root.join(&id);
        publish_tree_install(&target, &id, &entries, |digest| Ok(objects.join(digest))).unwrap();
        verify_tree_install(&target, &id, &entries).unwrap();
        assert_eq!(
            fs::read(target.join("payload/bin/renamed.dat")).unwrap(),
            b"base"
        );
        assert_eq!(
            fs::read(target.join("payload/localization/text.dat")).unwrap(),
            b"overlay"
        );
        assert!(target.join("payload/empty/nested").is_dir());
        assert_eq!(
            fs::read(target.join(".app-depot-installation-id")).unwrap(),
            id.as_bytes()
        );
        fs::write(target.join("payload/bin/renamed.dat"), b"edit").unwrap();
        assert_eq!(fs::read(target.join("payload/copy.dat")).unwrap(), b"base");
        assert_eq!(
            fs::read(objects.join(format!("{:x}", Sha256::digest(b"base")))).unwrap(),
            b"base"
        );
        assert_eq!(
            fs::read(objects.join(format!("{:x}", Sha256::digest(b"overlay")))).unwrap(),
            b"overlay"
        );
        assert!(
            publish_tree_install(&target, &id, &entries, |digest| Ok(objects.join(digest)))
                .is_err()
        );
        assert_eq!(
            fs::read(target.join("payload/bin/renamed.dat")).unwrap(),
            b"edit"
        );
    }

    #[test]
    fn manifest_layout_rejects_invalid_or_conflicting_destinations_before_staging() {
        let fixture = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let target = fixture.path().join(&id);
        for entries in [
            vec![layout_file("../outside", b"x")],
            vec![layout_file("C:relative", b"x")],
            vec![layout_file("NUL.txt", b"x")],
            vec![layout_file("stream:ads", b"x")],
            vec![layout_file("trailing.", b"x")],
            vec![layout_dir("Dir"), layout_dir("dir")],
            vec![layout_dir("same"), layout_file("same", b"x")],
            vec![layout_file("missing/child", b"x")],
            vec![
                layout_file("parent", b"x"),
                layout_file("parent/child", b"x"),
            ],
            vec![layout_file("same", b"x"), layout_file("same", b"x")],
        ] {
            assert!(
                publish_tree_install(&target, &id, &entries, |_| panic!(
                    "invalid map reached source lookup"
                ))
                .is_err()
            );
            assert_eq!(fixture.path().read_dir().unwrap().count(), 0);
        }
    }

    #[test]
    fn manifest_layout_failed_copy_does_not_publish_or_modify_sources() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let good = root.join("good");
        let corrupt = root.join("corrupt");
        fs::write(&good, b"good").unwrap();
        fs::write(&corrupt, b"bad!").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let target = root.join(&id);
        let entries = [
            layout_file("a-good", b"good"),
            layout_file("z-bad", b"want"),
        ];
        let good_hash = format!("{:x}", Sha256::digest(b"good"));
        assert!(
            publish_tree_install(&target, &id, &entries, |digest| {
                Ok(if digest == good_hash {
                    good.clone()
                } else {
                    corrupt.clone()
                })
            })
            .is_err()
        );
        assert!(!target.exists());
        assert_eq!(fs::read(&good).unwrap(), b"good");
        assert_eq!(fs::read(&corrupt).unwrap(), b"bad!");
        let stage = root
            .read_dir()
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".app-depot-install-")
            })
            .unwrap()
            .path();
        assert_eq!(fs::read(stage.join("payload/a-good")).unwrap(), b"good");
        assert!(!stage.join("payload/z-bad").exists());
        assert!(!stage.join(".app-depot-installation-id").exists());
    }

    #[test]
    fn publish_config_in_stage_with_parent_write_sharing_denied() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("object");
        fs::write(&source, b"data").unwrap();
        let chain = directory_chain(fixture.path(), false).unwrap();
        // A directory pin can deny a second write-open of the directory while
        // still allowing creation and rename of its children.
        let stage = open_child(
            chain.last().unwrap(),
            OsStr::new(".app-depot-tree-fixture"),
            DIRECTORY_ACCESS | GENERIC_WRITE | DELETE,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
            FILE_SHARE_READ,
        )
        .unwrap();
        let leaf = name_utf16(OsStr::new("Config.exe")).unwrap();
        publish_copy_at(&source, &stage, &leaf, 4, false, |source, out| {
            copy_then_verify(source, out, 4, |_| Ok(()))
        })
        .unwrap();
        assert_eq!(
            fs::read(fixture.path().join(".app-depot-tree-fixture/Config.exe")).unwrap(),
            b"data"
        );
    }

    #[test]
    fn source_contention_retries_without_recopying_and_reports_persistent_path() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("object");
        let target = fixture.path().join("published");
        fs::write(&source, b"data").unwrap();
        let writer = OpenOptions::new().write(true).open(&source).unwrap();
        let error =
            publish_verified_copy(&source, &target, 4, |_| panic!("source locked")).unwrap_err();
        for expected in ["NtCreateFile", "object", "os error 32"] {
            assert!(error.to_string().contains(expected), "{error}");
        }
        assert!(!target.exists());
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(writer);
        });
        publish_verified_copy(&source, &target, 4, |_| Ok(())).unwrap();
        release.join().unwrap();
        assert_eq!(fs::read(target).unwrap(), b"data");
    }

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
    fn quarantine_preserves_ntfs_ids_and_exposes_identical_replacements() {
        let fixture = tempfile::tempdir().unwrap();
        let live = fixture.path().join("installed");
        let quarantine = fixture.path().join("quarantine");
        ensure_directory(&live).unwrap();
        fs::write(live.join("payload"), b"verified bytes").unwrap();

        let identities = |path: &Path| {
            let chain = directory_chain(path, false).unwrap();
            let dir = chain.last().unwrap();
            let file = open_removal_file(dir, OsStr::new("payload")).unwrap();
            (file_identity(dir).unwrap(), file_identity(&file).unwrap())
        };

        let (root_id, payload_id) = identities(&live);
        rename_directory_noreplace(&live, &quarantine).unwrap();
        assert!(!live.exists());
        assert_eq!(identities(&quarantine), (root_id, payload_id));

        let moved = fixture.path().join("original");
        fs::rename(quarantine.join("payload"), &moved).unwrap();
        fs::copy(&moved, quarantine.join("payload")).unwrap();
        let (reopened_root_id, replacement_id) = identities(&quarantine);
        assert_eq!(reopened_root_id, root_id);
        assert_eq!(
            fs::read(quarantine.join("payload")).unwrap(),
            fs::read(&moved).unwrap()
        );
        assert_ne!(replacement_id, payload_id);
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
