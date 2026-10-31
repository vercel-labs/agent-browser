//! Launch owned Chrome in a Windows Job Object, assigning it at creation so
//! no child can escape between spawn and assignment. Only the daemon owns the
//! job handle: even terminating the daemon forcibly closes the job and kills
//! Chrome's entire process tree.
//!
//! Headless Chrome uses a private desktop in the current window station. Some
//! Chromium versions (including 150) let DWM draw backgrounds for hidden HWNDs
//! on the interactive desktop (#1498). A private desktop contains those
//! surfaces, including windows created later through CDP, without disabling
//! GPU rendering or changing headed/external browsers.

use std::ffi::{c_void, OsStr};
use std::fs::{File, OpenOptions};
use std::io;
use std::marker::PhantomData;
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateDesktopW, DESKTOP_CREATEWINDOW, DESKTOP_READOBJECTS, DESKTOP_WRITEOBJECTS,
    HDESK,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

/// Windows equivalent of the small `std::process::Child` surface Chrome uses.
/// The job is always terminated before its desktop and process handles close.
pub(super) struct Child {
    process: OwnedHandle,
    job: OwnedHandle,
    _desktop: Option<Desktop>,
    pid: u32,
    pub stderr: Option<File>,
}

struct Desktop(HDESK);

impl Drop for Desktop {
    fn drop(&mut self) {
        // SAFETY: This is our private desktop handle, never selected on a thread.
        unsafe { CloseDesktop(self.0) };
    }
}

impl Child {
    pub fn spawn(program: &Path, args: &[String], headless: bool) -> io::Result<Self> {
        // Resolve relative paths without relying on CreateProcess's executable
        // search rules. Chrome discovery and --executable-path supply a path.
        let program = std::fs::canonicalize(program)?;
        let application = wide(program.as_os_str())?;
        let mut command_line = quoted(program.as_os_str())?;
        for arg in args {
            command_line.push(b' ' as u16);
            command_line.extend(quoted(OsStr::new(arg))?);
        }
        command_line.push(0);

        // SAFETY: Null security attributes create an unnamed, non-inheritable
        // job. OwnedHandle closes it on every subsequent error path.
        let job = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: limits has the layout and size required by this info class.
        check(unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                size_of_val(&limits) as u32,
            )
        })?;

        let mut desktop_name = if headless {
            wide(OsStr::new(&format!(
                "agent-browser-{}",
                uuid::Uuid::new_v4()
            )))?
        } else {
            Vec::new()
        };
        let desktop = if headless {
            // SAFETY: The unique name is terminated; the default DACL applies.
            // No switch-desktop permission is requested or used.
            let handle = unsafe {
                CreateDesktopW(
                    desktop_name.as_ptr(),
                    null(),
                    null(),
                    0,
                    DESKTOP_CREATEWINDOW | DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS,
                    null(),
                )
            };
            if handle == 0 {
                return Err(io::Error::last_os_error());
            }
            Some(Desktop(handle))
        } else {
            None
        };

        let null_file = OpenOptions::new().read(true).write(true).open("NUL")?;
        let (stderr_reader, stderr_writer) = io::pipe()?;
        let null_handle = inheritable(null_file.as_raw_handle() as HANDLE)?;
        let stderr_handle = inheritable(stderr_writer.as_raw_handle() as HANDLE)?;
        // The handle allowlist excludes the job, the desktop, and all other
        // daemon handles, even when another thread is spawning concurrently.
        let handles = [raw(&null_handle), raw(&stderr_handle)];
        let jobs = [raw(&job)];
        let mut attributes = AttributeList::new(2)?;
        attributes.add(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
        attributes.add(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;

        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = size_of_val(&startup) as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = raw(&null_handle);
        startup.StartupInfo.hStdOutput = raw(&null_handle);
        startup.StartupInfo.hStdError = raw(&stderr_handle);
        startup.StartupInfo.lpDesktop = if headless {
            desktop_name.as_mut_ptr()
        } else {
            null_mut()
        };
        startup.lpAttributeList = attributes.as_ptr();
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: All strings, attribute values and inherited handles stay
        // alive through CreateProcessW. The environment and cwd are inherited,
        // as with the normal Chrome Command. Job assignment is atomic with
        // process creation, including when our daemon is killed during spawn.
        check(unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &startup.StartupInfo,
                &mut info,
            )
        })?;
        // SAFETY: Successful CreateProcessW transfers these two valid handles
        // to us. The main thread handle is not needed after atomic job setup.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess as *mut c_void) };
        let _thread = unsafe { OwnedHandle::from_raw_handle(info.hThread as *mut c_void) };
        Ok(Self {
            process,
            job,
            _desktop: desktop,
            pid: info.dwProcessId,
            stderr: Some(File::from(OwnedHandle::from(stderr_reader))),
        })
    }

    pub fn id(&self) -> u32 {
        self.pid
    }

    pub fn kill(&mut self) -> io::Result<()> {
        // SAFETY: This job contains only the Chrome tree we launched.
        check(unsafe { TerminateJobObject(raw(&self.job), 1) })
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.wait_for(0)
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.wait_for(INFINITE)?
            .ok_or_else(|| io::Error::other("Unexpected timeout waiting for Chrome"))
    }

    fn wait_for(&self, timeout: u32) -> io::Result<Option<ExitStatus>> {
        // SAFETY: process remains owned for both API calls. Wait before reading
        // the exit code so a process exiting with STILL_ACTIVE (259) is reaped.
        match unsafe { WaitForSingleObject(raw(&self.process), timeout) } {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                check(unsafe { GetExitCodeProcess(raw(&self.process), &mut code) })?;
                Ok(Some(ExitStatus::from_raw(code)))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }
}

/// Attribute storage must be aligned and must outlive CreateProcessW.
struct AttributeList<'a>(Vec<usize>, PhantomData<&'a [HANDLE]>);

impl<'a> AttributeList<'a> {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: The first call queries the required allocation size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        check(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut bytes)
        })?;
        Ok(Self(storage, PhantomData))
    }

    fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }

    fn add(&mut self, key: u32, handles: &'a [HANDLE]) -> io::Result<()> {
        // SAFETY: Callers retain the handle arrays until after CreateProcessW.
        check(unsafe {
            UpdateProcThreadAttribute(
                self.as_ptr(),
                0,
                key as usize,
                handles.as_ptr().cast(),
                size_of_val(handles),
                null_mut(),
                null(),
            )
        })
    }
}

impl Drop for AttributeList<'_> {
    fn drop(&mut self) {
        // SAFETY: Only initialized lists are constructed; storage is still live.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle() as HANDLE
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle == 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: Called only for newly created, non-null handles.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as *mut c_void) })
    }
}

fn inheritable(handle: HANDLE) -> io::Result<OwnedHandle> {
    let mut duplicate = 0;
    // SAFETY: Duplicate into our process; ownership is transferred below.
    check(unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    })?;
    owned(duplicate)
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in Chrome argument",
        ));
    }
    value.push(0);
    Ok(value)
}

/// Quote a single argument for Chrome's Windows C runtime. Backslashes are
/// doubled only before a quote or the closing quote, preserving paths/JSON.
fn quoted(value: &OsStr) -> io::Result<Vec<u16>> {
    let value = wide(value)?;
    let mut output = vec![b'"' as u16];
    let mut slashes = 0;
    for &ch in &value[..value.len() - 1] {
        if ch == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(b'\\' as u16, slashes));
        if ch == b'"' as u16 {
            output.extend(std::iter::repeat_n(b'\\' as u16, slashes + 1));
        }
        output.push(ch);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::StationsAndDesktops::{
        GetThreadDesktop, GetUserObjectInformationW, UOI_NAME,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentThreadId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    const TEST_DIR: &str = "AGENT_BROWSER_TEST_WINDOWS_PROCESS_DIR";

    fn helper_args(name: &str) -> Vec<String> {
        vec![
            "--exact".into(),
            format!("native::cdp::windows_process::tests::{name}"),
            "--ignored".into(),
            "--nocapture".into(),
        ]
    }

    fn test_dir() -> std::path::PathBuf {
        std::env::var_os(TEST_DIR)
            .expect("internal helper environment")
            .into()
    }

    fn desktop_name() -> String {
        let mut name = [0u16; 256];
        let mut needed = 0;
        // SAFETY: The returned desktop handle is borrowed, not closed here.
        check(unsafe {
            GetUserObjectInformationW(
                GetThreadDesktop(GetCurrentThreadId()),
                UOI_NAME,
                name.as_mut_ptr().cast(),
                size_of_val(&name) as u32,
                &mut needed,
            )
        })
        .unwrap();
        String::from_utf16_lossy(&name[..name.iter().position(|&ch| ch == 0).unwrap()])
    }

    fn wait_for_file(path: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.is_empty() {
                    return text;
                }
            }
            assert!(
                Instant::now() < deadline,
                "Timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn process(pid: u32) -> OwnedHandle {
        owned(unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        })
        .unwrap()
    }

    fn assert_exited(handle: &OwnedHandle) {
        assert_eq!(
            unsafe { WaitForSingleObject(raw(handle), 5000) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    #[ignore = "internal subprocess helper"]
    fn leaf_helper() {
        std::fs::write(test_dir().join("leaf.pid"), std::process::id().to_string()).unwrap();
        // Bound helper lifetime even if the parent test panics.
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[ignore = "internal subprocess helper"]
    fn tree_helper() {
        std::fs::write(test_dir().join("desktop.txt"), desktop_name()).unwrap();
        let mut leaf = Command::new(std::env::current_exe().unwrap())
            .args(helper_args("leaf_helper"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for_file(&test_dir().join("leaf.pid"));
        eprintln!("tree ready");
        if test_dir().join("exit-parent").exists() {
            return;
        }
        leaf.wait().unwrap();
    }

    #[test]
    #[ignore = "internal subprocess helper"]
    fn owner_helper() {
        let mut child = Child::spawn(
            &std::env::current_exe().unwrap(),
            &helper_args("tree_helper"),
            true,
        )
        .unwrap();
        std::fs::write(test_dir().join("root.pid"), child.id().to_string()).unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn private_desktop_and_tree_cleanup() {
        let guard = EnvGuard::new(&[TEST_DIR]);
        for headless in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            guard.set(TEST_DIR, dir.path().to_str().unwrap());
            let mut child = Child::spawn(
                &std::env::current_exe().unwrap(),
                &helper_args("tree_helper"),
                headless,
            )
            .unwrap();
            let actual_desktop = wait_for_file(&dir.path().join("desktop.txt"));
            if headless {
                assert!(actual_desktop.starts_with("agent-browser-"));
                assert_ne!(actual_desktop, desktop_name());
            } else {
                assert_eq!(actual_desktop, desktop_name());
            }
            let leaf_pid: u32 = wait_for_file(&dir.path().join("leaf.pid")).parse().unwrap();
            let leaf = process(leaf_pid);
            let root = process(child.id());
            assert!(child.try_wait().unwrap().is_none());
            let mut stderr = child.stderr.take().unwrap();
            drop(child);
            assert_exited(&root);
            assert_exited(&leaf);
            let mut output = String::new();
            stderr.read_to_string(&mut output).unwrap();
        }
    }

    #[test]
    fn cleanup_includes_descendants_after_browser_exits() {
        let guard = EnvGuard::new(&[TEST_DIR]);
        let dir = tempfile::tempdir().unwrap();
        guard.set(TEST_DIR, dir.path().to_str().unwrap());
        std::fs::write(dir.path().join("exit-parent"), "1").unwrap();
        let mut child = Child::spawn(
            &std::env::current_exe().unwrap(),
            &helper_args("tree_helper"),
            false,
        )
        .unwrap();
        let leaf_pid: u32 = wait_for_file(&dir.path().join("leaf.pid")).parse().unwrap();
        let leaf = process(leaf_pid);
        assert!(child.wait().unwrap().success());
        assert_eq!(unsafe { WaitForSingleObject(raw(&leaf), 0) }, WAIT_TIMEOUT);
        drop(child);
        assert_exited(&leaf);
    }

    #[test]
    fn killing_owner_reaps_tree_without_touching_unrelated_process() {
        let dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let mut other = Command::new(std::env::current_exe().unwrap())
            .args(helper_args("leaf_helper"))
            .env(TEST_DIR, other_dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args(helper_args("owner_helper"))
            .env(TEST_DIR, dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let root_pid: u32 = wait_for_file(&dir.path().join("root.pid")).parse().unwrap();
        let root = process(root_pid);
        let leaf_pid: u32 = wait_for_file(&dir.path().join("leaf.pid")).parse().unwrap();
        let leaf = process(leaf_pid);
        owner.kill().unwrap();
        owner.wait().unwrap();
        assert_exited(&root);
        assert_exited(&leaf);
        assert!(other.try_wait().unwrap().is_none());
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[test]
    fn quotes_preserve_paths_quotes_and_empty_arguments() {
        for (input, expected) in [
            ("", "\"\""),
            ("plain", "\"plain\""),
            (
                r"C:\profile with spaces\",
                "\"C:\\profile with spaces\\\\\"",
            ),
            ("a\"b", "\"a\\\"b\""),
            ("a\\\"b", "\"a\\\\\\\"b\""),
            ("你好", "\"你好\""),
        ] {
            assert_eq!(
                String::from_utf16(&quoted(OsStr::new(input)).unwrap()).unwrap(),
                expected
            );
        }
        assert!(quoted(OsStr::new("a\0b")).is_err());
    }

    #[test]
    fn missing_executable_fails_without_starting_process() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Child::spawn(&dir.path().join("missing.exe"), &[], true).is_err());
    }
}
