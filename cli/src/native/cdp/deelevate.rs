//! Windows: spawn Chrome unelevated from an elevated daemon.
//!
//! ## Why this exists
//!
//! When the agent-browser daemon runs from a UAC-elevated process and
//! spawns Chrome via the normal `Command::spawn` path, Chrome (M138+)
//! detects the unnecessary elevation and tries to relaunch itself
//! unelevated by duplicating Explorer's medium-integrity token. The
//! original Chrome process exits cleanly and a grandchild takes over.
//! The daemon's `Child` handle no longer points at the live Chrome, so
//! `Child::try_wait` reports the exit before the grandchild can write
//! `DevToolsActivePort` -- surfacing as the misleading "Chrome exited
//! early (exit code: 0) without writing DevToolsActivePort".
//!
//! Beyond breaking the daemon's process tracking, the elevated launch
//! also breaks Chrome's renderer sandbox for Chrome for Testing
//! installations: the install ACL doesn't grant `Users` rights, so the
//! low-integrity sandbox can't load the executable and the GPU process
//! cycle-crashes.
//!
//! ## What this module does
//!
//! All Windows-specific elevation handling is contained here so that
//! `chrome.rs` stays platform-neutral. We spawn Chrome with the same
//! medium-integrity token Chrome would have used internally: open
//! Explorer's primary token, duplicate it as a primary token, and call
//! `CreateProcessWithTokenW`. The resulting Chrome process runs unelevated
//! with the user's standard token, the renderer sandbox can load the
//! binary because integrity matches, and `MaybeAutoDeElevate` no-ops
//! because the user account is no longer "unnecessarily elevated."
//!
//! This is the same sequence as Chromium's `base::win::RunDeElevated`.

#![cfg(windows)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, TokenElevationType,
    TokenElevationTypeFull, TokenPrimary, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessWithTokenW, GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    TerminateProcess, WaitForSingleObject, PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION,
    STARTUPINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

use super::chrome::{read_devtools_active_port, ChromeChild};

const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const STILL_ACTIVE: u32 = 259;
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether `try_launch_chrome` should spawn Chrome unelevated instead of via
/// the normal `Command::spawn` path. True only when the daemon is running
/// with `TokenElevationTypeFull` (a UAC consent elevation) AND Explorer is
/// available to borrow a medium-integrity token from.
///
/// If Explorer isn't running (Server Core, some RDP/container setups), we
/// can't de-elevate -- but Chrome's own `MaybeAutoDeElevate` can't either
/// (it uses the same `GetShellWindow` lookup), so the normal spawn path is
/// no worse off than before this fix. Returning false there keeps the
/// fallback behavior identical to the pre-fix code.
pub fn should_de_elevate() -> bool {
    is_unnecessarily_elevated() && explorer_is_available()
}

fn explorer_is_available() -> bool {
    // SAFETY: GetShellWindow takes no arguments and returns a window handle
    // or NULL; always safe to call.
    unsafe { GetShellWindow() != 0 }
}

/// True when this process is running with `TokenElevationTypeFull`, the
/// same condition Chromium's `UserAccountIsUnnecessarilyElevated` checks.
/// False for the always-on built-in Administrator account
/// (`TokenElevationTypeDefault`), for ordinary unelevated users
/// (`TokenElevationTypeLimited`), and when querying the token fails.
pub fn is_unnecessarily_elevated() -> bool {
    let mut token: HANDLE = 0;
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if opened == 0 || token == 0 {
        return false;
    }

    let mut elevation_type: i32 = 0;
    let mut returned: u32 = 0;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevationType,
            &mut elevation_type as *mut i32 as *mut _,
            std::mem::size_of::<i32>() as u32,
            &mut returned,
        )
    };
    unsafe { CloseHandle(token) };

    ok != 0 && elevation_type == TokenElevationTypeFull
}

/// A Chrome process spawned with Explorer's medium-integrity token.
///
/// We hold a `HANDLE` instead of a `std::process::Child` because there's no
/// public Rust API to construct a `Child` from a foreign handle. It
/// implements [`ChromeChild`] so `ChromeProcess` can treat it identically to
/// a normally-spawned `Child`.
pub struct UnelevatedChrome {
    handle: HANDLE,
    pid: u32,
}

// `ChromeChild` requires `Send + Sync` (the daemon holds the process in a
// `tokio::spawn`ed task). Both fields are plain integers (`HANDLE` is `isize`
// in windows-sys), so the auto-derived traits hold. Assert it at compile time
// so a future field that isn't `Send + Sync` fails the build here rather than
// at the distant `tokio::spawn` call site.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<UnelevatedChrome>();
};

impl UnelevatedChrome {
    fn exit_code(&self) -> Option<i32> {
        if self.handle == 0 {
            return Some(0);
        }
        let mut code: u32 = 0;
        let ok = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        if ok == 0 || code == STILL_ACTIVE {
            None
        } else {
            Some(code as i32)
        }
    }
}

impl ChromeChild for UnelevatedChrome {
    fn id(&self) -> u32 {
        self.pid
    }

    fn has_exited(&mut self) -> bool {
        self.exit_code().is_some()
    }

    fn kill(&mut self) {
        if self.handle == 0 {
            return;
        }
        // SAFETY: handle is a live process handle with PROCESS_TERMINATE.
        unsafe {
            TerminateProcess(self.handle, 1);
            // Wait briefly so the process tree can flush to disk.
            WaitForSingleObject(self.handle, 2000);
        }
    }
}

impl Drop for UnelevatedChrome {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe { CloseHandle(self.handle) };
            self.handle = 0;
        }
    }
}

/// Spawn Chrome unelevated via Explorer's token and wait for it to publish
/// its DevTools endpoint. Returns the process handle and the resolved
/// `ws://` URL. Caller owns the user-data-dir lifecycle (cleanup on error).
pub fn launch_unelevated(
    chrome_path: &Path,
    args: &[String],
    user_data_dir: &Path,
) -> std::io::Result<(UnelevatedChrome, String)> {
    let mut child = spawn_unelevated(chrome_path, args)?;

    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    let poll_interval = Duration::from_millis(50);

    // No stderr fallback here (the foreign-handle spawn doesn't pipe stderr);
    // DevToolsActivePort polling is the reliable signal on Windows anyway.
    while Instant::now() <= deadline {
        if let Some(code) = child.exit_code() {
            return Err(io_err(format!(
                "Chrome exited early (exit code: {}) without writing DevToolsActivePort",
                code
            )));
        }
        if let Some((port, ws_path)) = read_devtools_active_port(user_data_dir) {
            return Ok((child, format!("ws://127.0.0.1:{}{}", port, ws_path)));
        }
        std::thread::sleep(poll_interval);
    }

    child.kill();
    Err(io_err("Timeout waiting for DevToolsActivePort".to_string()))
}

/// Spawn `chrome_path` with `args` using Explorer's primary token.
fn spawn_unelevated(chrome_path: &Path, args: &[String]) -> std::io::Result<UnelevatedChrome> {
    // 1. Locate Explorer
    let shell_hwnd: HWND = unsafe { GetShellWindow() };
    if shell_hwnd == 0 {
        return Err(io_err(
            "GetShellWindow returned NULL (no Explorer running?)".to_string(),
        ));
    }
    let mut shell_pid: u32 = 0;
    let _ = unsafe { GetWindowThreadProcessId(shell_hwnd, &mut shell_pid) };
    if shell_pid == 0 {
        return Err(io_err(
            "GetWindowThreadProcessId for shell window failed".to_string(),
        ));
    }

    // 2. Open Explorer process and its token
    let shell_proc = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, 0, shell_pid) };
    if shell_proc == 0 {
        return Err(last_error("OpenProcess(Explorer)"));
    }
    let _shell_proc_guard = HandleGuard(shell_proc);

    let mut shell_tok: HANDLE = 0;
    if unsafe { OpenProcessToken(shell_proc, TOKEN_DUPLICATE, &mut shell_tok) } == 0 {
        return Err(last_error("OpenProcessToken(Explorer)"));
    }
    let _shell_tok_guard = HandleGuard(shell_tok);

    // 3. Duplicate as a primary token suitable for CreateProcessWithTokenW
    let dup_rights = TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_DUPLICATE
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID;
    let mut primary_tok: HANDLE = 0;
    if unsafe {
        DuplicateTokenEx(
            shell_tok,
            dup_rights,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary_tok,
        )
    } == 0
    {
        return Err(last_error("DuplicateTokenEx"));
    }
    let _primary_tok_guard = HandleGuard(primary_tok);

    // 4. Build command line; quote the program path and any args containing
    //    whitespace. CreateProcessWithTokenW takes a single mutable wide
    //    command-line string (the API may modify the buffer).
    let cmd_line = build_command_line(chrome_path, args);
    let app_w = to_wide(chrome_path.as_os_str());
    let mut cmd_w = to_wide(OsStr::new(&cmd_line));

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let ok = unsafe {
        CreateProcessWithTokenW(
            primary_tok,
            0,
            app_w.as_ptr(),
            cmd_w.as_mut_ptr(),
            CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(last_error("CreateProcessWithTokenW"));
    }

    // We don't need the thread handle.
    unsafe { CloseHandle(pi.hThread) };

    Ok(UnelevatedChrome {
        handle: pi.hProcess,
        pid: pi.dwProcessId,
    })
}

fn to_wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Build a quoted Windows command line for `CreateProcessWithTokenW`.
///
/// Quoting rule: wrap a token in double quotes if it contains whitespace or
/// is empty; escape embedded double quotes by preceding them with a
/// backslash, and double up backslashes that precede an escaped quote.
/// This matches the rules the C runtime uses to parse `argv`.
fn build_command_line(program: &Path, args: &[String]) -> String {
    let mut s = String::new();
    quote(&mut s, &program.to_string_lossy());
    for a in args {
        s.push(' ');
        quote(&mut s, a);
    }
    s
}

fn quote(out: &mut String, arg: &str) {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|c| c == ' ' || c == '\t' || c == '\n' || c == '\x0b' || c == '"');
    if !needs_quotes {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        if c == '"' {
            for _ in 0..(backslashes * 2 + 1) {
                out.push('\\');
            }
            out.push('"');
            backslashes = 0;
            continue;
        }
        for _ in 0..backslashes {
            out.push('\\');
        }
        backslashes = 0;
        out.push(c);
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
}

fn last_error(ctx: &str) -> std::io::Error {
    let code = unsafe { GetLastError() };
    std::io::Error::other(format!("{} failed: GetLastError={}", ctx, code))
}

fn io_err(msg: String) -> std::io::Error {
    std::io::Error::other(msg)
}

struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_simple() {
        let mut s = String::new();
        quote(&mut s, "hello");
        assert_eq!(s, "hello");
    }

    #[test]
    fn quote_with_spaces() {
        let mut s = String::new();
        quote(&mut s, "hello world");
        assert_eq!(s, "\"hello world\"");
    }

    #[test]
    fn quote_with_quote() {
        let mut s = String::new();
        quote(&mut s, "say \"hi\"");
        assert_eq!(s, "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn quote_trailing_backslash() {
        let mut s = String::new();
        quote(&mut s, "C:\\path with space\\");
        assert_eq!(s, "\"C:\\path with space\\\\\"");
    }

    #[test]
    fn build_command_line_basic() {
        let cl = build_command_line(
            Path::new("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"),
            &["--foo".to_string(), "--bar=hello world".to_string()],
        );
        assert_eq!(
            cl,
            "\"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe\" --foo \"--bar=hello world\""
        );
    }

    #[test]
    fn is_unnecessarily_elevated_does_not_panic() {
        // Just exercises the syscalls. The return value depends on the test
        // environment so we don't assert it.
        let _ = is_unnecessarily_elevated();
        let _ = should_de_elevate();
    }
}
