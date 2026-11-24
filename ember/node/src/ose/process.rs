//! Process ownership for the OSE server and its one-shot extension commands.

use std::io;
use std::process::{Child, Command, ExitStatus};

pub(super) struct ManagedChild {
    pub(super) child: Child,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

impl ManagedChild {
    pub(super) fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(windows)]
        return windows::spawn(command);

        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(command, 0);
            Ok(Self {
                child: command.spawn()?,
            })
        }
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        #[cfg(unix)]
        {
            // SAFETY: the child leads its own group, even if the launcher has exited.
            unsafe {
                libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use std::ptr::null;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
    };

    pub(super) fn spawn(command: &mut Command) -> io::Result<ManagedChild> {
        // SAFETY: null attributes give a private, non-inheritable handle to a new unnamed job.
        let raw_job = unsafe { CreateJobObjectW(null(), null()) };
        if raw_job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a fresh owned handle.
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the job and the correctly sized limits structure live across the call.
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // Start suspended so the launcher cannot create a child before it joins the job.
        let child = command.creation_flags(CREATE_SUSPENDED).spawn()?;
        let managed = ManagedChild {
            child,
            job: Some(job),
        };
        let job = managed.job.as_ref().expect("job was just set");
        // SAFETY: both handles are owned by managed and support assignment.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), managed.child.as_raw_handle()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        resume_initial_thread(managed.child.id())?;
        Ok(managed)
    }

    // Rust's stable Child exposes the process handle. Find the initial thread while it is
    // suspended, when it is still the only thread belonging to the new process.
    fn resume_initial_thread(pid: u32) -> io::Result<()> {
        // SAFETY: the flag takes a system-wide snapshot and ignores the second argument.
        let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if raw_snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the snapshot call returned a new handle.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot) };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        // SAFETY: the snapshot is valid and the record size is initialized as required.
        let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: the id comes from the snapshot and OpenThread returns a fresh handle.
                let raw_thread =
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if raw_thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: OpenThread succeeded and the handle supports resuming the thread.
                let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread) };
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            // SAFETY: the snapshot remains valid and owns no reference to the entry.
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "cannot find the initial OSE thread",
        ))
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    const PID_FILE: &str = "EMBER_TEST_OSE_TREE_PID";
    const EXIT_LAUNCHER: &str = "EMBER_TEST_OSE_EXIT_LAUNCHER";

    fn fixture(test: &str) -> Command {
        let suite = module_path!().split_once("::").unwrap().1;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", &format!("{suite}::{test}")]);
        command
    }

    #[test]
    fn fr_w4_windows_job_launcher_fixture() {
        let Some(path) = std::env::var_os(PID_FILE) else {
            return;
        };
        let descendant = fixture("fr_w4_windows_job_descendant_fixture")
            .spawn()
            .unwrap();
        std::fs::write(path, descendant.id().to_string()).unwrap();
        if std::env::var_os(EXIT_LAUNCHER).is_none() {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn fr_w4_windows_job_descendant_fixture() {
        if std::env::var_os(PID_FILE).is_some() {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn fr_w4_windows_ose_kills_the_tree_even_if_the_launcher_exits_first() {
        for launcher_exits in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let path: PathBuf = tmp.path().join("descendant.pid");
            let mut command = fixture("fr_w4_windows_job_launcher_fixture");
            command.env(PID_FILE, &path);
            if launcher_exits {
                command.env(EXIT_LAUNCHER, "1");
            }
            let mut managed = ManagedChild::spawn(&mut command).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let pid = loop {
                if let Some(pid) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                {
                    break pid;
                }
                assert!(
                    Instant::now() < deadline,
                    "launcher did not record its descendant"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            // SAFETY: pid was returned by a successful spawn in the launcher fixture.
            let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
            assert!(
                !raw.is_null(),
                "cannot open descendant process: {}",
                io::Error::last_os_error()
            );
            // SAFETY: OpenProcess returned a new owned handle.
            let descendant = unsafe { OwnedHandle::from_raw_handle(raw) };
            if launcher_exits {
                while managed.try_wait().unwrap().is_none() {
                    assert!(Instant::now() < deadline, "launcher did not exit");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            drop(managed);
            // SAFETY: the descendant handle is still valid and has synchronization access.
            let status = unsafe { WaitForSingleObject(descendant.as_raw_handle(), 3000) };
            if status != WAIT_OBJECT_0 {
                // SAFETY: cleanup for a failing test, using its own process handle.
                unsafe {
                    TerminateProcess(descendant.as_raw_handle(), 1);
                }
            }
            assert_eq!(status, WAIT_OBJECT_0, "OSE descendant survived cleanup");
        }
    }
}
