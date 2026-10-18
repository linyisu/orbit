//! Windows process ownership without a spawn-to-job assignment race.

use std::{
    io,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};

use windows_sys::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent},
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, OpenThread, ResumeThread,
            THREAD_SUSPEND_RESUME,
        },
    },
};

/// Own a private job; descendants cannot opt out of its cleanup boundary.
pub struct ProcessGroup {
    job: Option<OwnedHandle>,
    leader: u32,
}

impl ProcessGroup {
    /// Start suspended, assign the job, then resume. Failure never runs the command unowned.
    /// This owns the command's Windows creation flags, including its console process group.
    pub fn spawn(
        command: &mut tokio::process::Command,
    ) -> io::Result<(tokio::process::Child, Self)> {
        // SAFETY: null security attributes create a non-inheritable, unnamed job.
        let job = owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the live job and initialized structure remain valid for this call.
        check(unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        let mut child = command
            .creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP)
            .kill_on_drop(true)
            .spawn()?;
        let leader = child
            .id()
            .ok_or_else(|| io::Error::other("missing child PID"))?;
        let group = Self {
            job: Some(job),
            leader,
        };
        let assigned = (|| {
            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("missing child handle"))?;
            // SAFETY: both handles are owned and live; the child has not executed yet.
            check(unsafe { AssignProcessToJobObject(group.handle(), process) })?;
            resume_primary_thread(leader)
        })();
        if let Err(error) = assigned {
            // kill-on-drop also covers failure before the child enters the job.
            let _ = child.start_kill();
            return Err(error);
        }
        Ok((child, group))
    }

    /// Request console cleanup when a shared console exists; callers retain a kill deadline.
    pub fn request_termination(&self) {
        if self.job.is_some() {
            // SAFETY: the nonzero PID targets only the process group created by spawn.
            unsafe {
                GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.leader);
            }
        }
    }

    /// Terminate the job, then close its sole non-inheritable handle as a cleanup fallback.
    pub fn kill(&mut self) {
        if let Some(job) = self.job.take() {
            // SAFETY: this live handle owns the private job, never a caller-selected PID.
            unsafe {
                TerminateJobObject(job.as_raw_handle(), 1);
            }
        }
    }

    fn handle(&self) -> HANDLE {
        self.job
            .as_ref()
            .expect("live process job")
            .as_raw_handle()
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: each caller passes a newly created handle whose ownership is transferred here.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn resume_primary_thread(process: u32) -> io::Result<()> {
    // std::process does not expose the primary thread handle. The suspended
    // child cannot create other threads before we attach it to the job.
    // SAFETY: the snapshot request has no pointer arguments.
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure is writable and initialized with its required size.
    check(unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) })?;
    loop {
        if entry.th32OwnerProcessID == process {
            // SAFETY: the entry came from the snapshot; we request only resume access.
            let thread =
                owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) })?;
            // SAFETY: the live thread handle carries THREAD_SUSPEND_RESUME access.
            let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
            return match previous {
                1 => Ok(()),
                u32::MAX => Err(io::Error::last_os_error()),
                _ => Err(io::Error::other("unexpected child suspension state")),
            };
        }
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        // SAFETY: snapshot and writable entry remain valid throughout enumeration.
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } == 0 {
            return Err(io::Error::other("suspended child thread not found"));
        }
    }
}
