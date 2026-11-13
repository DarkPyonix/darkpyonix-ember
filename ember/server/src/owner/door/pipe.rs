//! The claim door on Windows: a named pipe (SPEC `FR-S12`; INTENT D37 "Owner presence", #815).
//!
//! The pipe is `\\.\pipe\ember-owner-<hash>` ([`crate::owner::pipe_name`]). Its security
//! descriptor has one access entry, the server's user (SDDL `D:P(A;;GA;;;<user SID>)`, a
//! protected DACL, so nothing is inherited), and it refuses remote clients
//! (`PIPE_REJECT_REMOTE_CLIENTS`, tokio's default, set here again on purpose). The first
//! instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a second server on the same
//! folder, or a process that took the name first, makes the bind fail instead of sharing it.
//!
//! The peer checks are those of the unix door: the claimant's pid from
//! `GetNamedPipeClientProcessId`, its token user compared with the server's, and the parent walk
//! over the process snapshot (`CreateToolhelp32Snapshot`), which ends at a parent that has
//! exited ([`super::tree_verdict_exited_parents`]). The protocol is the same code.
//!
//! What the access list does not stop: a process of the server's own user may create another
//! instance of the pipe and answer a client in the server's place, as it may replace
//! `owner.sock` on unix. The console code is shown by the real server only, so such an impostor
//! gets a key only by relaying a code the owner typed.
//!
//! Written on macOS and not compiled there: every call is checked against the sources of
//! windows-sys 0.61.2 and tokio 1.53.1; the Windows CI job builds and runs it.

use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::watch;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, TokenUser, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, WaitNamedPipeW};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::{
    check_tree, refuse, serve_split, tree_verdict_exited_parents, DoorConfig, Inner, Proc, ProcTable,
    MAX_CONNECTIONS,
};
use crate::owner::Owner;

/// A handle closed on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned open by the call that made this value and is closed
        // once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// A buffer `LocalFree` releases on drop (what the SDDL conversions allocate).
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer was allocated by a call documented to need LocalFree.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

/// A NUL-terminated UTF-16 string read into a `String`.
///
/// # Safety
/// `p` points at a NUL-terminated UTF-16 string.
unsafe fn from_wide(p: *const u16) -> String {
    let mut n = 0;
    // SAFETY: the caller promises a terminating NUL.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: the `n` units before the NUL are readable.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The `TOKEN_USER` of a process token, in a buffer aligned for it.
struct TokenUserBuf(Vec<u64>);

impl TokenUserBuf {
    fn sid(&self) -> PSID {
        // SAFETY: the buffer holds a TOKEN_USER written by GetTokenInformation and is aligned
        // to 8 bytes.
        unsafe { (*(self.0.as_ptr() as *const TOKEN_USER)).User.Sid }
    }
}

/// The user of the process behind `process` (a handle with query rights).
fn user_of(process: HANDLE) -> io::Result<TokenUserBuf> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is written on success only.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut len = 0u32;
    // SAFETY: a size query: no buffer, `len` receives the size needed.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer holds at least `len` bytes.
    if unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(TokenUserBuf(buf))
}

fn my_user() -> io::Result<TokenUserBuf> {
    // SAFETY: the pseudo handle of this process needs no closing and cannot fail.
    user_of(unsafe { GetCurrentProcess() })
}

/// The SID of the user this process runs as, as a string (`S-1-5-21-...`).
pub fn current_user_sid() -> io::Result<String> {
    let me = my_user()?;
    let mut s: *mut u16 = null_mut();
    // SAFETY: `me.sid()` is a valid SID; `s` is LocalFree'd below.
    if unsafe { ConvertSidToStringSidW(me.sid(), &mut s) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _free = Local(s.cast());
    // SAFETY: ConvertSidToStringSidW wrote a NUL-terminated string.
    Ok(unsafe { from_wide(s) })
}

/// True when the process `pid` runs as this process's user. An error when its token cannot be
/// read.
fn same_user(pid: u32) -> io::Result<bool> {
    // SAFETY: OpenProcess returns null on failure and a handle we own on success.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    let h = Handle(h);
    let theirs = user_of(h.0)?;
    let mine = my_user()?;
    // SAFETY: both SIDs are valid for the life of their buffers.
    Ok(unsafe { EqualSid(mine.sid(), theirs.sid()) } != 0)
}

/// The full path of the executable of `pid`, when it can be read.
fn image_name(pid: u32) -> Option<String> {
    // SAFETY: as in `same_user`.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let h = Handle(h);
    let mut buf = vec![0u16; 32 * 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` units; `len` receives the length written, without the NUL.
    if unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// The process table from the process snapshot: pid to parent and executable name. Empty when
/// the snapshot cannot be taken, which refuses every claim.
pub fn snapshot_table() -> ProcTable {
    // SAFETY: no preconditions; INVALID_HANDLE_VALUE on failure.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        tracing::warn!("owner door: no process snapshot: {}", io::Error::last_os_error());
        return ProcTable::new();
    }
    let snap = Handle(snap);
    let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut out = ProcTable::new();
    // SAFETY: `e.dwSize` is set as the API requires.
    let mut more = unsafe { Process32FirstW(snap.0, &mut e) } != 0;
    while more {
        let n = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
        out.insert(
            e.th32ProcessID,
            Proc { ppid: e.th32ParentProcessID, exe: String::from_utf16_lossy(&e.szExeFile[..n]) },
        );
        // SAFETY: as above.
        more = unsafe { Process32NextW(snap.0, &mut e) } != 0;
    }
    out
}

/// The DACL of the pipe behind `handle` (either end), as SDDL. For tests: the pipe's access list
/// as Windows applied it, generic rights mapped (`GA` reads back as `FA`).
pub fn pipe_dacl(handle: RawHandle) -> io::Result<String> {
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: only the descriptor is asked for; it is LocalFree'd below.
    let err = unsafe {
        GetSecurityInfo(
            handle as HANDLE,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    let _sd = Local(sd);
    let mut s: *mut u16 = null_mut();
    // SAFETY: `sd` is a valid descriptor; `s` is LocalFree'd below.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut s,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let _s = Local(s.cast());
    // SAFETY: the conversion wrote a NUL-terminated string.
    Ok(unsafe { from_wide(s) })
}

/// True while a pipe named `name` exists, whether or not an instance is free. It does not
/// connect.
pub fn pipe_exists(name: &str) -> bool {
    let w = wide(name);
    // SAFETY: `w` is NUL-terminated. One millisecond: only "exists" is asked.
    if unsafe { WaitNamedPipeW(w.as_ptr(), 1) } != 0 {
        return true;
    }
    // SAFETY: no preconditions.
    unsafe { GetLastError() } != ERROR_FILE_NOT_FOUND
}

/// The pipe's security descriptor: the current user only. Kept for the life of the door, as
/// every instance is created with it.
struct PipeSecurity(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is read only, by CreateNamedPipeW, after it is built.
unsafe impl Send for PipeSecurity {}
// SAFETY: as above.
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    fn current_user() -> io::Result<PipeSecurity> {
        // P: protected, nothing inherited. One entry: generic all for this user.
        let sddl = wide(&format!("D:P(A;;GA;;;{})", current_user_sid()?));
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `sddl` is NUL-terminated; `sd` is LocalFree'd in Drop.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(PipeSecurity(sd))
    }

    /// One pipe instance. `first`: fail if the pipe exists already.
    fn create(&self, name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        };
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(first).reject_remote_clients(true);
        // SAFETY: `sa` is a valid SECURITY_ATTRIBUTES for the call, and its descriptor lives
        // in `self`.
        unsafe { opts.create_with_security_attributes_raw(name, (&mut sa as *mut SECURITY_ATTRIBUTES).cast::<c_void>()) }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// The bound claim pipe. Dropping it closes the pipe and voids every key.
pub struct OwnerDoor {
    name: String,
    path: PathBuf,
    owner: Arc<Owner>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl OwnerDoor {
    /// Creates the pipe of `data_dir` ([`crate::owner::pipe_name`]) and starts accepting claims.
    /// A pipe of that name that exists already (a running server, or a process that took the
    /// name first) is an error. Must be called inside a tokio runtime.
    pub fn bind(data_dir: &Path, owner: Arc<Owner>, cfg: DoorConfig) -> io::Result<OwnerDoor> {
        let name = crate::owner::pipe_name(data_dir);
        let security = Arc::new(PipeSecurity::current_user()?);
        let first = security.create(&name, true).map_err(|e| {
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("{name} is held by a running server, or another process created it first"),
                )
            } else {
                e
            }
        })?;
        let (stop_tx, stop_rx) = watch::channel(false);
        let inner = Arc::new(Inner {
            owner: owner.clone(),
            cfg,
            pending: Mutex::new(HashMap::new()),
            connections: tokio::sync::Semaphore::new(MAX_CONNECTIONS),
            stop: stop_rx,
        });
        let task = tokio::spawn(accept_loop(first, name.clone(), security, inner));
        Ok(OwnerDoor { path: PathBuf::from(&name), name, owner, stop: stop_tx, task })
    }

    /// The pipe's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The pipe's name as a path, for the startup log (the unix door logs its socket path).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OwnerDoor {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.task.abort();
        self.owner.void_all();
    }
}

async fn accept_loop(mut server: NamedPipeServer, name: String, security: Arc<PipeSecurity>, inner: Arc<Inner>) {
    loop {
        let connected = server.connect().await;
        // The next instance listens before this one is handed on. A client that comes in the
        // gap gets ERROR_PIPE_BUSY and retries.
        let next = loop {
            match security.create(&name, false) {
                Ok(s) => break s,
                Err(e) => {
                    tracing::warn!("owner door: cannot create a pipe instance: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        };
        let this = std::mem::replace(&mut server, next);
        match connected {
            Ok(()) => {
                let inner = inner.clone();
                tokio::spawn(async move { serve_conn(inner, this).await });
            }
            Err(e) => tracing::warn!("owner door: pipe connect failed: {e}"),
        }
    }
}

/// The claimant behind `pipe`, or why it gets no key.
fn check_peer(inner: &Inner, pipe: &NamedPipeServer) -> Result<(u32, String), &'static str> {
    let mut pid = 0u32;
    // SAFETY: the handle is the connected pipe's and lives as long as `pipe`.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) } == 0 || pid == 0 {
        return Err("no_peer_pid");
    }
    match same_user(pid) {
        Ok(true) => {}
        Ok(false) => return Err("other_user"),
        Err(e) => {
            tracing::warn!(pid, "owner claim refused: the claimant's user cannot be read: {e}");
            return Err("no_peer_credentials");
        }
    }
    let table = (inner.cfg.procs)();
    let roots = (inner.cfg.roots)();
    check_tree(pid, tree_verdict_exited_parents(pid, &table, &roots))?;
    let exe = image_name(pid).or_else(|| table.get(&pid).map(|p| p.exe.clone())).unwrap_or_default();
    Ok((pid, exe))
}

async fn serve_conn(inner: Arc<Inner>, mut pipe: NamedPipeServer) {
    let Ok(_permit) = inner.connections.try_acquire() else {
        refuse(&mut pipe, "busy").await;
        return;
    };
    let peer = check_peer(&inner, &pipe);
    let (r, w) = tokio::io::split(pipe);
    serve_split(&inner, r, w, peer).await;
}
