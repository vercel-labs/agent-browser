//! Private, atomic writes for browser credentials and saved session state.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

pub(crate) fn private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("Failed to create private directory: {error}"))?;
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_dir()
    {
        return Err(format!(
            "Private directory is not a real directory: {}",
            path.display()
        ));
    }
    restrict(path, true)
}

pub(crate) fn private_file(path: &Path) -> Result<(), String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Err(format!(
            "Credential path is not a regular file: {}",
            path.display()
        ));
    }
    restrict(path, false)
}

#[cfg(unix)]
fn restrict(path: &Path, directory: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
    .map_err(|error| {
        format!(
            "Failed to restrict permissions on {}: {error}",
            path.display()
        )
    })
}

#[cfg(windows)]
fn restrict(path: &Path, _directory: bool) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::{
        SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    // Replace inherited access with owner and SYSTEM access, including children.
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(format!(
                "Failed to build private ACL: {}",
                std::io::Error::last_os_error()
            ));
        }
        let result = SetFileSecurityW(
            name.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        let error = std::io::Error::last_os_error();
        LocalFree(descriptor as _);
        if result == 0 {
            return Err(format!(
                "Failed to restrict permissions on {}: {error}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Stage in the same directory and restrict access before writing any secrets.
/// Existing files remain intact if staging or promotion fails.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_private_inner(path, bytes, true).map(|_| ())
}

pub(crate) fn write_private_new(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    write_private_inner(path, bytes, false)
}

fn write_private_inner(path: &Path, bytes: &[u8], replace: bool) -> Result<bool, String> {
    if path.symlink_metadata().is_ok() {
        private_file(path)?;
        if !replace {
            return Ok(false);
        }
    }
    let name = path
        .file_name()
        .ok_or("Credential file has no name")?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<bool, String> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
        private_file(&temporary)?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        if replace {
            fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        } else {
            // Publish a fully written key without overwriting a concurrent winner.
            match fs::hard_link(&temporary, path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Ok(false)
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(true)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|error| {
        format!(
            "Failed to save private credentials to {}: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_never_replaces_an_existing_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("key");
        assert!(write_private_new(&path, b"first").unwrap());
        assert!(!write_private_new(&path, b"second").unwrap());
        assert_eq!(fs::read(path).unwrap(), b"first");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn private_write_replaces_contents_and_cleans_staging_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        write_private(&path, b"old").unwrap();
        write_private(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn credentials_are_private_even_in_shared_export_directory() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, b"secret").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
            0o777
        );
        private_dir(directory.path()).unwrap();
        assert_eq!(
            fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_writes_refuse_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original");
        fs::write(&original, b"keep").unwrap();
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&original, &link).unwrap();
        assert!(write_private(&link, b"replace").is_err());
        assert_eq!(fs::read(original).unwrap(), b"keep");
        assert!(private_dir(&link).is_err());
    }
}
