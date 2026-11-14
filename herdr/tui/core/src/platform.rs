//! Process, executable and filesystem operations for the host platform.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::{
    fs::{OpenOptionsExt, PermissionsExt},
    process::CommandExt,
};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT},
    System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject},
    System::Threading::*,
};

pub fn alive(pid: u64) -> bool {
    #[cfg(unix)]
    {
        pid > 0 && unsafe { libc::kill(pid as i32, 0) } == 0
    }
    #[cfg(windows)]
    {
        let Some(pid) = u32::try_from(pid).ok().filter(|pid| *pid > 0) else { return false };
        // SAFETY: the handle is checked, queried into a live local, and closed once.
        unsafe {
            let handle = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            // The exit code 259 is also STILL_ACTIVE, so only the wait tells an exited process from a running one.
            let active = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
            CloseHandle(handle);
            active
        }
    }
}

/// The executable file of a running process.
#[cfg(windows)]
pub fn image(pid: u64) -> Option<PathBuf> {
    let pid = u32::try_from(pid).ok().filter(|pid| *pid > 0)?;
    // SAFETY: the handle is checked, the buffer outlives the call and `len` is its capacity, and the handle is closed once.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(handle);
        ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

pub fn terminate(pid: u64) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    if pid > 0 {
        taskkill_tree(pid);
    }
}

/// Windowless processes have no graceful signal, so stopping one always kills its tree.
#[cfg(windows)]
fn taskkill_tree(pid: u64) {
    let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).stdout(std::process::Stdio::null()).creation_flags(CREATE_NO_WINDOW).status();
}

pub fn kill(pid: u64) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let Some(pid) = u32::try_from(pid).ok().filter(|pid| *pid > 0) else { return };
        // SAFETY: only a successfully opened handle is terminated and closed.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !handle.is_null() {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
}

pub fn signal_group(pid: u64, force: bool) {
    if pid > 0 {
        #[cfg(unix)]
        {
            let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
            // SAFETY: the group is the run's own, started with process_group(0).
            unsafe { libc::kill(-(pid as i32), sig) };
        }
        #[cfg(windows)]
        {
            let _ = force;
            if let Some(job) = take_job(pid) {
                // SAFETY: the job handle is ours, from `spawn_in_job`, and is closed once.
                unsafe {
                    TerminateJobObject(job as HANDLE, 1);
                    CloseHandle(job as HANDLE);
                }
            }
            taskkill_tree(pid);
        }
    }
}

/// Windows job handles of the processes `spawn_in_job` started, by pid.
#[cfg(windows)]
static JOBS: std::sync::Mutex<Vec<(u64, usize)>> = std::sync::Mutex::new(Vec::new());

#[cfg(windows)]
fn take_job(pid: u64) -> Option<usize> {
    let mut jobs = JOBS.lock().unwrap_or_else(|e| e.into_inner());
    jobs.iter().position(|(p, _)| *p == pid).map(|i| jobs.swap_remove(i).1)
}

/// Starts `cmd` in a job of its own, so `signal_group` ends everything it started. MSYS `sh` forks through intermediate
/// processes, which breaks the parent links `taskkill /T` follows. On Windows the child starts suspended and runs only once
/// it is in the job, so nothing it forks escapes; on Unix this is a plain `spawn`, the group does the work.
pub fn spawn_in_job(cmd: &mut Command) -> io::Result<std::process::Child> {
    #[cfg(unix)]
    {
        cmd.spawn()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let child = cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_SUSPENDED).spawn()?;
        let pid = child.id();
        // SAFETY: the job is checked and either kept in JOBS or closed here; the child handle is owned by `child`.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if !job.is_null() && AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) != 0 {
                JOBS.lock().unwrap_or_else(|e| e.into_inner()).push((u64::from(pid), job as usize));
            } else {
                eprintln!("cockpit: process {pid} runs outside a job: {}", io::Error::last_os_error());
                if !job.is_null() {
                    CloseHandle(job);
                }
            }
        }
        if let Err(e) = resume(pid) {
            let mut child = child;
            let _ = child.kill();
            release(u64::from(pid));
            return Err(e);
        }
        Ok(child)
    }
}

/// Resumes the threads of a process created suspended.
#[cfg(windows)]
fn resume(pid: u32) -> io::Result<()> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next};
    // SAFETY: the snapshot and each thread handle are checked and closed once; the entry is a live local with its size set.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = false;
        let mut more = Thread32First(snapshot, &mut entry) != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if !thread.is_null() {
                    resumed |= ResumeThread(thread) != u32::MAX;
                    CloseHandle(thread);
                }
            }
            more = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        if resumed { Ok(()) } else { Err(io::Error::other(format!("no thread of process {pid} could be resumed"))) }
    }
}

/// One row of the process table, as `ps -o pid=,ppid=,%cpu=,rss=` gives it.
#[cfg(windows)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    /// CPU time over the process's lifetime, in percent of one core (what ps's %cpu is on Linux).
    pub cpu: f64,
    pub rss_kb: u64,
}

/// Every process, with the CPU and memory of those this one may query; the others read 0.
#[cfg(windows)]
pub fn processes() -> Vec<Process> {
    use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    let mut found = Vec::new();
    // SAFETY: the snapshot and each process handle are checked and closed once; every struct is a live local with its size set.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut now: FILETIME = std::mem::zeroed();
        GetSystemTimeAsFileTime(&mut now);
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let mut process = Process { pid: entry.th32ProcessID, ppid: entry.th32ParentProcessID, cpu: 0.0, rss_kb: 0 };
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process.pid);
            if !handle.is_null() {
                let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) = std::mem::zeroed();
                if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0 {
                    let lived = ticks(now).saturating_sub(ticks(created));
                    if lived > 0 {
                        process.cpu = (ticks(kernel) + ticks(user)) as f64 / lived as f64 * 100.0;
                    }
                }
                let mut memory: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
                memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
                if K32GetProcessMemoryInfo(handle, &mut memory, memory.cb) != 0 {
                    process.rss_kb = memory.WorkingSetSize as u64 / 1024;
                }
                CloseHandle(handle);
            }
            found.push(process);
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    found
}

/// Runs `handler` when the console is interrupted, broken, closed or the system shuts down; the handler ends the process.
#[cfg(windows)]
pub fn on_console_shutdown(handler: impl Fn() + Send + Sync + 'static) {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler};
    static HANDLER: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();
    extern "system" fn ctrl(event: u32) -> i32 {
        match (event, HANDLER.get()) {
            (CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_SHUTDOWN_EVENT, Some(handler)) => {
                handler();
                1
            }
            _ => 0,
        }
    }
    if HANDLER.set(Box::new(handler)).is_ok() {
        // SAFETY: `ctrl` is a plain function that lives for the whole process.
        unsafe { SetConsoleCtrlHandler(Some(ctrl), 1) };
    }
}

/// Forgets the job of a process that ended; its leftovers are not killed.
pub fn release(pid: u64) {
    #[cfg(windows)]
    if let Some(job) = take_job(pid) {
        // SAFETY: the job handle is ours, from `spawn_in_job`, and is closed once.
        unsafe { CloseHandle(job as HANDLE) };
    }
    #[cfg(unix)]
    let _ = pid;
}

/// Adds `args` to `cmd`. On Windows each is quoted for the MSYS `sh`, which loses the quotes inside an argument that has no space.
/// Every argument given to a command that starts `shell()` goes through here.
pub fn args<I, S>(cmd: &mut Command, args: I) -> &mut Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    #[cfg(unix)]
    {
        cmd.args(args)
    }
    #[cfg(windows)]
    {
        for arg in args {
            cmd.raw_arg(quote(&arg.as_ref().to_string_lossy()));
        }
        cmd
    }
}

/// Quotes one argument as the Windows command line rules ask: backslashes double only before a quote.
#[cfg(windows)]
fn quote(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

pub fn new_group(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        cmd.process_group(0)
    }
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
    }
}

pub fn detach(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        // SAFETY: setsid only detaches the child from the terminal, between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        // Command duplicates configured stdio; the caller's original pipes must not keep its output open in the child.
        // Verified by `a_detached_spawn_leaves_later_children_their_parents_stdout`: clearing the flag does not stop later children
        // from inheriting this process's stdout, because `Stdio::inherit` and the default duplicate the handle as inheritable.
        for handle in [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
            // SAFETY: these are this process's standard handles; clearing inheritance does not close them.
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
        // DETACHED_PROCESS would make Windows ignore CREATE_NO_WINDOW and give every console grandchild a visible window.
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
    }
}

/// Fills the bytes from the Windows system random source.
#[cfg(windows)]
pub fn random_bytes(bytes: &mut [u8]) -> std::io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let len = u32::try_from(bytes.len()).map_err(std::io::Error::other)?;
    // SAFETY: bytes points to len writable bytes; the system provider needs no algorithm handle.
    let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), len, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status < 0 {
        return Err(std::io::Error::other(format!("BCryptGenRandom failed: {status:#x}")));
    }
    Ok(())
}

pub fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    {
        path.is_file()
            && path.extension().is_some_and(|ext| extensions().iter().any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&ext.to_string_lossy())))
    }
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    #[cfg(unix)]
    {
        std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| executable(p))
    }
    #[cfg(windows)]
    {
        std::env::split_paths(&path).find_map(|dir| {
            let bare = dir.join(name);
            if executable(&bare) {
                return Some(bare);
            }
            extensions().iter().map(|ext| dir.join(format!("{name}{ext}"))).find(|p| executable(p))
        })
    }
}

#[cfg(windows)]
fn extensions() -> Vec<String> {
    std::env::var("PATHEXT").unwrap_or_else(|_| ".exe;.cmd;.bat;.com".into()).split(';').filter(|s| !s.is_empty()).map(String::from).collect()
}

pub fn private_file(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        opts.mode(0o600)
    }
    #[cfg(windows)]
    {
        opts
    } // The profile directory's ACL protects private files.
}

pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(windows)]
    {
        let _ = (path, mode);
        Ok(())
    }
}

pub fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        // mklink treats forward slashes in directory names as switches.
        let target: PathBuf = target.components().collect();
        let link: PathBuf = link.components().collect();
        let status = Command::new("cmd").args(["/c", "mklink", "/J"]).arg(link).arg(target).creation_flags(CREATE_NO_WINDOW).status()?;
        if status.success() { Ok(()) } else { Err(io::Error::other("directory junction failed")) }
    }
}

/// A command for `program` and its `args`. The program `sh` is the platform shell and gets its arguments through `args`.
pub fn command<I, S>(program: &str, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    if program != "sh" {
        let mut command = Command::new(program);
        command.args(args);
        return command;
    }
    let mut command = Command::new(shell());
    self::args(&mut command, args);
    command
}

/// The shell to run scripts with. On Windows it is the MSYS `sh`: pass its arguments through `args`, never `arg` or `args`.
pub fn shell() -> PathBuf {
    #[cfg(unix)]
    {
        "sh".into()
    }
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("COCKPIT_SH").filter(|p| !p.is_empty()) {
            return path.into();
        }
        if let Some(root) = std::env::var_os("ProgramFiles") {
            for rel in ["Git/usr/bin/sh.exe", "Git/bin/sh.exe"] {
                let path = PathBuf::from(&root).join(rel);
                if path.is_file() {
                    return path;
                }
            }
        }
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                if wsl_path(&dir) {
                    continue;
                }
                let shell = dir.join("sh.exe");
                if shell.is_file() {
                    return shell;
                }
            }
        }
        "sh".into()
    }
}

#[cfg(windows)]
fn wsl_path(path: &Path) -> bool {
    let path = path.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()).replace('/', "\\").to_ascii_lowercase();
    path == system || path.starts_with(&format!("{}\\", system.trim_end_matches('\\'))) || path.split('\\').any(|part| part == "windowsapps")
}

pub fn exe_name(base: &str) -> String {
    #[cfg(unix)]
    {
        base.into()
    }
    #[cfg(windows)]
    {
        format!("{base}.exe")
    }
}

/// The console's echo of typed input, off while it lives; nothing changes when stdin is not a console.
#[cfg(windows)]
pub struct EchoOff(Option<(windows_sys::Win32::Foundation::HANDLE, u32)>);

#[cfg(windows)]
impl EchoOff {
    pub fn on_stdin() -> EchoOff {
        // SAFETY: the standard input handle is owned by the process and stays open.
        Self::on(unsafe { windows_sys::Win32::System::Console::GetStdHandle(windows_sys::Win32::System::Console::STD_INPUT_HANDLE) })
    }

    fn on(input: HANDLE) -> EchoOff {
        use windows_sys::Win32::System::Console::{ENABLE_ECHO_INPUT, GetConsoleMode, SetConsoleMode};
        let mut mode = 0;
        // SAFETY: `mode` is a live local; GetConsoleMode fails, and nothing is changed, for a handle that is not a console.
        unsafe {
            if input.is_null() || input == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE || GetConsoleMode(input, &mut mode) == 0 {
                return EchoOff(None);
            }
            SetConsoleMode(input, mode & !ENABLE_ECHO_INPUT);
        }
        EchoOff(Some((input, mode)))
    }
}

#[cfg(windows)]
impl Drop for EchoOff {
    fn drop(&mut self) {
        if let Some((input, mode)) = self.0 {
            // SAFETY: puts back the mode GetConsoleMode read from the same handle.
            unsafe { windows_sys::Win32::System::Console::SetConsoleMode(input, mode) };
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The child half of the test below: installs the handler, says it is ready, and waits to be broken.
    fn shutdown_child(dir: &Path) {
        let marker = dir.join("handled");
        on_console_shutdown(move || {
            let _ = std::fs::write(&marker, "");
            std::process::exit(0);
        });
        std::fs::write(dir.join("ready"), "").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
        std::process::exit(3);
    }

    #[test]
    fn a_console_break_runs_the_shutdown_handler_and_the_process_exits_cleanly() {
        use windows_sys::Win32::System::Console::{AllocConsole, CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, GetConsoleProcessList};
        if let Some(dir) = std::env::var_os("COCKPIT_SHUTDOWN_CHILD") {
            shutdown_child(Path::new(&dir));
        }
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: the pid list is a live local; AllocConsole only gives a console to a process that has none.
        unsafe {
            let mut list = [0u32; 1];
            if GetConsoleProcessList(list.as_mut_ptr(), 1) == 0 {
                AllocConsole();
            }
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::tests::a_console_break_runs_the_shutdown_handler_and_the_process_exits_cleanly", "--nocapture"])
            .env("COCKPIT_SHUTDOWN_CHILD", dir.path())
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        while !dir.path().join("ready").exists() && started.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(dir.path().join("ready").exists(), "the child installed its handler");
        // SAFETY: a break event for the child's own process group.
        assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) }, 0, "{}", io::Error::last_os_error());
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > std::time::Duration::from_secs(15) {
                let _ = child.kill();
                panic!("the child did not end after the break");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!(status.code(), Some(0));
        assert!(dir.path().join("handled").exists());
    }

    #[test]
    fn the_process_table_holds_this_process_with_its_memory_and_parent() {
        let me = processes().into_iter().find(|p| p.pid == std::process::id()).expect("this process is listed");
        assert!(me.rss_kb > 0 && me.ppid > 0 && me.cpu >= 0.0, "rss {} ppid {} cpu {}", me.rss_kb, me.ppid, me.cpu);
    }

    #[test]
    fn echo_goes_off_on_a_console_and_comes_back_and_a_pipe_is_left_alone() {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Console::{ENABLE_ECHO_INPUT, GetConsoleMode};
        let mode = |h: HANDLE| {
            let mut mode = 0;
            // SAFETY: `mode` is a live local.
            (unsafe { GetConsoleMode(h, &mut mode) } != 0).then_some(mode)
        };
        let (pipe, _writer) = std::io::pipe().unwrap();
        assert!(EchoOff::on(pipe.as_raw_handle() as HANDLE).0.is_none());
        // Only a process with a console can open CONIN$; without one this half is checked by hand.
        let Ok(console) = OpenOptions::new().read(true).write(true).open("CONIN$") else { return };
        let handle = console.as_raw_handle() as HANDLE;
        let before = mode(handle).unwrap();
        let off = EchoOff::on(handle);
        assert_eq!(mode(handle).unwrap() & ENABLE_ECHO_INPUT, 0);
        drop(off);
        assert_eq!(mode(handle), Some(before));
    }

    #[test]
    fn liveness_checks_the_process_handle() {
        assert!(alive(u64::from(std::process::id())));
        assert!(!alive(0));
        assert!(!alive(u64::MAX));
    }

    #[test]
    fn executables_need_a_pathext_extension() {
        let dir = tempfile::tempdir().unwrap();
        let ext = extensions().into_iter().next().unwrap();
        let binary = dir.path().join(format!("cockpit{ext}"));
        std::fs::write(&binary, "").unwrap();
        assert!(executable(&binary));
        assert!(!executable(dir.path()));
        let plain = dir.path().join("cockpit");
        std::fs::write(&plain, "").unwrap();
        assert!(!executable(&plain));
        assert_eq!(exe_name("cockpit"), "cockpit.exe");
    }

    #[test]
    fn shell_search_rejects_wsl_paths() {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        assert!(wsl_path(&PathBuf::from(root).join("System32")));
        assert!(wsl_path(Path::new("C:/acme/WindowsApps")));
        assert!(!wsl_path(Path::new("C:/acme/Git/usr/bin")));
    }

    fn wait_dead(pid: u64) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(3) {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    fn sleeper() -> std::process::Child {
        let mut cmd = Command::new("ping");
        cmd.args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null());
        new_group(&mut cmd).spawn().unwrap()
    }

    #[test]
    fn terminate_and_graceful_signal_end_a_windowless_process() {
        let mut child = sleeper();
        terminate(u64::from(child.id()));
        assert!(wait_dead(u64::from(child.id())));
        let _ = child.wait();
        let mut child = sleeper();
        signal_group(u64::from(child.id()), false);
        assert!(wait_dead(u64::from(child.id())));
        let _ = child.wait();
    }

    const DETACH_PROBE: &str = "COCKPIT_TEST_DETACH_PROBE";

    #[test]
    fn detach_probe() {
        use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
        use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;
        let Some(out) = std::env::var_os(DETACH_PROBE) else { return };
        // SAFETY: plain queries on this process's own console.
        let (attached, visible) = unsafe {
            let mut pids = [0u32; 4];
            let window = GetConsoleWindow();
            (GetConsoleProcessList(pids.as_mut_ptr(), 4) > 0, !window.is_null() && IsWindowVisible(window) != 0)
        };
        std::fs::write(out, format!("{attached} {visible}")).unwrap();
    }

    #[test]
    fn a_detached_child_has_a_console_but_no_visible_window() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("probe.txt");
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["platform::tests::detach_probe", "--exact", "--nocapture"]).env(DETACH_PROBE, &out);
        detach(&mut cmd).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        assert!(cmd.status().unwrap().success());
        assert_eq!(std::fs::read_to_string(out).unwrap(), "true false");
    }

    #[test]
    fn quoting_follows_the_command_line_rules() {
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("plain"), r#""plain""#);
        assert_eq!(quote(r"a\b"), r#""a\b""#);
        assert_eq!(quote(r"a\\"), r#""a\\\\""#);
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote(r#"{"k":["v"]}"#), r#""{\"k\":[\"v\"]}""#);
    }

    #[test]
    fn quotes_inside_a_space_free_argument_reach_the_shell() {
        // Unquoted `$IFS` splits into words, so `echo` gets its argument only while the shell still sees the quotes.
        let out = args(&mut Command::new(shell()), ["-c", "echo$IFS\"a\""]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "a");
    }

    #[test]
    fn a_process_that_exited_with_259_is_not_alive() {
        let mut child = Command::new("cmd").args(["/c", "exit 259"]).spawn().unwrap();
        let pid = u64::from(child.id());
        assert_eq!(child.wait().unwrap().code(), Some(259));
        assert!(!alive(pid));
    }

    fn read_pid(path: &Path) -> Option<u64> {
        std::fs::read_to_string(path).ok().and_then(|t| t.trim().parse().ok())
    }

    #[test]
    fn a_process_forked_at_the_first_line_is_still_in_the_job() {
        let dir = tempfile::tempdir().unwrap();
        for round in 0..10 {
            let file = dir.path().join(format!("winpid-{round}"));
            let body = format!("sleep 60 & cat /proc/$!/winpid > {}; sleep 60", file.to_string_lossy().replace('\\', "/"));
            let mut cmd = Command::new(shell());
            args(&mut cmd, ["-c", &body]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            let mut child = spawn_in_job(&mut cmd).unwrap();
            let run = u64::from(child.id());
            let started = std::time::Instant::now();
            let forked = loop {
                if let Some(pid) = read_pid(&file) {
                    break pid;
                }
                assert!(started.elapsed() < std::time::Duration::from_secs(10), "round {round}: the run never forked");
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            signal_group(run, true);
            assert!(wait_dead(run), "round {round}: the run outlived its stop");
            assert!(wait_dead(forked), "round {round}: the fork outlived its stop");
            let _ = child.wait();
        }
    }

    const INHERIT_PROBE: &str = "COCKPIT_TEST_INHERIT_PROBE";

    #[test]
    fn inherit_probe() {
        if std::env::var_os(INHERIT_PROBE).is_none() {
            return;
        }
        let mut detached = Command::new("cmd");
        detached.args(["/c", "exit 0"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        assert!(detach(&mut detached).status().unwrap().success());
        Command::new("cmd").args(["/c", "echo explicit"]).stdout(std::process::Stdio::inherit()).status().unwrap();
        Command::new("cmd").args(["/c", "echo implicit"]).status().unwrap();
    }

    #[test]
    fn a_detached_spawn_leaves_later_children_their_parents_stdout() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["platform::tests::inherit_probe", "--exact", "--nocapture"])
            .env(INHERIT_PROBE, "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("explicit") && text.contains("implicit"), "stdout was {text:?}");
    }
}
