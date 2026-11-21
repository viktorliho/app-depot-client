//! Minimal process handoff for a user-owned AppImage. No library or Server access.

#[cfg(target_os = "linux")]
mod linux {
    use sha2::{Digest, Sha256};
    use std::{
        env,
        fs::{self, File, OpenOptions},
        io::{self, Read},
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    fn digest(path: &Path) -> io::Result<String> {
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 256 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    fn owned_regular(path: &Path) -> io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::other("update path is not an owned regular file"));
        }
        Ok(())
    }

    fn exchange(a: &Path, b: &Path) -> io::Result<()> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let a = CString::new(a.as_os_str().as_bytes())?;
        let b = CString::new(b.as_os_str().as_bytes())?;
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn sync_parent(path: &Path) -> io::Result<()> {
        File::open(path.parent().ok_or_else(|| io::Error::other("no parent"))?)?.sync_all()
    }

    fn create_backup(target: &Path, backup: &Path, expected_sha: &str) -> io::Result<()> {
        let mut created = false;
        let result = (|| {
            let mut source = File::open(target)?;
            let mode = source.metadata()?.permissions().mode();
            let mut destination = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(backup)?;
            created = true;
            io::copy(&mut source, &mut destination)?;
            destination.set_permissions(fs::Permissions::from_mode(mode))?;
            destination.sync_all()?;
            if digest(backup)? != expected_sha {
                return Err(io::Error::other(
                    "previous-version backup verification failed",
                ));
            }
            sync_parent(backup)
        })();
        if result.is_err() && created {
            let _ = fs::remove_file(backup);
        }
        result
    }

    fn appimage(path: &Path) -> io::Result<()> {
        let mut header = [0; 11];
        File::open(path)?.read_exact(&mut header)?;
        if &header[..4] == b"\x7fELF" && &header[8..11] == b"AI\x02" {
            Ok(())
        } else {
            Err(io::Error::other("update is not a Type 2 AppImage"))
        }
    }

    fn launch(target: &Path, marker: &Path) -> io::Result<std::process::Child> {
        let mut command = Command::new(target);
        command
            .arg("--appdepot-update-ack")
            .arg(marker)
            .env_remove("APPIMAGE")
            .env_remove("APPDIR")
            .env_remove("ARGV0")
            .env_remove("OWD")
            .env_remove("LD_LIBRARY_PATH")
            .stdin(Stdio::null());
        command.spawn()
    }

    fn run() -> io::Result<()> {
        let args: Vec<_> = env::args_os().collect();
        if args.len() != 8 || args[1] != "--apply" {
            return Err(io::Error::other(
                "expected --apply TARGET STAGED BACKUP PID OLD_SHA NEW_SHA",
            ));
        }
        let target = PathBuf::from(&args[2]);
        let staged = PathBuf::from(&args[3]);
        let backup = PathBuf::from(&args[4]);
        let pid: u32 = args[5]
            .to_string_lossy()
            .parse()
            .map_err(|_| io::Error::other("invalid pid"))?;
        let old_sha = args[6].to_string_lossy();
        let new_sha = args[7].to_string_lossy();
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("missing parent"))?;
        if !target.is_absolute()
            || staged.parent() != Some(parent)
            || backup.parent() != Some(parent)
            || target == staged
            || target == backup
            || staged == backup
            || !fs::symlink_metadata(parent)?.is_dir()
            || fs::symlink_metadata(parent)?.uid() != unsafe { libc::geteuid() }
            || fs::symlink_metadata(&backup).is_ok()
        {
            return Err(io::Error::other("unsafe update destination"));
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        while unsafe { libc::kill(pid as i32, 0) } == 0 {
            if Instant::now() >= deadline {
                return Err(io::Error::other("launcher did not exit in time"));
            }
            thread::sleep(Duration::from_millis(100));
        }
        owned_regular(&target)?;
        owned_regular(&staged)?;
        appimage(&target)?;
        appimage(&staged)?;
        if digest(&target)? != old_sha || digest(&staged)? != new_sha {
            return Err(io::Error::other(
                "AppImage changed since update verification",
            ));
        }
        create_backup(&target, &backup, &old_sha)?;
        exchange(&target, &staged)?;
        sync_parent(&target)?;
        let marker = parent.join(format!(".appdepot-update-ack-{}", std::process::id()));
        let mut child = match launch(&target, &marker) {
            Ok(child) => child,
            Err(error) => {
                exchange(&target, &staged)?;
                sync_parent(&target)?;
                let _ = fs::remove_file(&backup);
                return Err(error);
            }
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() && Instant::now() < deadline {
            if child.try_wait()?.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if marker.exists() {
            fs::remove_file(marker)?;
            fs::remove_file(&staged)?;
            sync_parent(&target)?;
            return Ok(());
        }
        let _ = child.kill();
        let _ = child.wait();
        exchange(&target, &staged)?;
        sync_parent(&target)?;
        let _ = fs::remove_file(&backup);
        let _ = launch(&target, &marker);
        Err(io::Error::other(
            "updated application did not acknowledge startup; previous version restored",
        ))
    }

    pub(super) fn main() {
        let result = run();
        if let Err(error) = &result {
            if let Some(parent) = env::args_os()
                .nth(2)
                .and_then(|path| PathBuf::from(path).parent().map(Path::to_path_buf))
            {
                let report = parent.join(".appdepot-update-error");
                if let Ok(mut file) = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(report)
                {
                    use std::io::Write;
                    let _ = writeln!(file, "{error}");
                    let _ = file.sync_all();
                }
            }
            eprintln!("App Depot update: {error}");
        }
        if let Ok(executable) = env::current_exe()
            && executable.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with(".appdepot-update-helper-")
            })
        {
            let _ = fs::remove_file(executable);
        }
        if result.is_err() {
            std::process::exit(1);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn atomic_exchange_preserves_both_versions() {
            let temp = tempfile::tempdir().unwrap();
            let old = temp.path().join("old");
            let new = temp.path().join("new");
            fs::write(&old, b"old").unwrap();
            fs::write(&new, b"new").unwrap();
            exchange(&old, &new).unwrap();
            assert_eq!(fs::read(old).unwrap(), b"new");
            assert_eq!(fs::read(new).unwrap(), b"old");
        }
        #[test]
        fn backup_never_overwrites_an_existing_file() {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let backup = temp.path().join("backup");
            fs::write(&source, b"previous").unwrap();
            fs::write(&backup, b"do not replace").unwrap();
            let digest = digest(&source).unwrap();
            assert!(create_backup(&source, &backup, &digest).is_err());
            assert_eq!(fs::read(&source).unwrap(), b"previous");
            assert_eq!(fs::read(&backup).unwrap(), b"do not replace");
        }
        #[test]
        fn previous_version_survives_interruption_after_exchange() {
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("installed");
            let staged = temp.path().join("staged");
            let backup = temp.path().join("backup");
            fs::write(&target, b"working previous version").unwrap();
            fs::write(&staged, b"replacement version").unwrap();
            let old_hash = digest(&target).unwrap();
            create_backup(&target, &backup, &old_hash).unwrap();
            exchange(&target, &staged).unwrap();
            assert_eq!(fs::read(&target).unwrap(), b"replacement version");
            assert_eq!(fs::read(&backup).unwrap(), b"working previous version");
            assert_eq!(digest(&backup).unwrap(), old_hash);
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("The App Depot update helper supports Linux AppImages only.");
    std::process::exit(2);
}
