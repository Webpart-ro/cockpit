//! A new bin/ built beside the old one, then swapped in at once, so a space app starting meanwhile finds one or the
//! other whole; running processes keep the binary they started from.

use cockpit_core::platform;
#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Unpacks the release's archive as bin/, noting its version.
pub fn unpack(dir: &Path, archive: &[u8], version: &str) -> Result<(), String> {
    let staging = Staging::new(dir)?;
    tar::Archive::new(flate2::read::GzDecoder::new(archive)).unpack(&staging.0).map_err(|_| "unreadable archive".to_string())?;
    if !staging.0.join(platform::exe_name("cockpit")).is_file() {
        return Err("no cockpit binary in the archive".into());
    }
    for entry in std::fs::read_dir(&staging.0).map_err(|e| e.to_string())?.flatten() {
        if entry.path().is_file() {
            let _ = platform::set_mode(&entry.path(), 0o755);
        }
    }
    staging.install(version)
}

/// Puts the running binary in as bin/cockpit, on its own version, for a folder whose release is not out yet.
pub fn copy_own(dir: &Path, version: &str) -> Result<(), String> {
    let staging = Staging::new(dir)?;
    let own = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::copy(&own, staging.0.join(platform::exe_name("cockpit"))).map_err(|e| format!("copy of {} failed: {e}", own.display()))?;
    staging.install(version)
}

/// A folder beside bin/, removed unless it was swapped in.
struct Staging(PathBuf);

impl Staging {
    fn new(dir: &Path) -> Result<Self, String> {
        let path = dir.join(format!("bin.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).map_err(|e| format!("cannot write in {}: {e}", dir.display()))?;
        Ok(Staging(path))
    }

    /// Notes the version, then swaps the folder in as bin/; the old bin/ ends up here and goes with it.
    fn install(self, version: &str) -> Result<(), String> {
        std::fs::write(self.0.join(".version"), format!("{version}\n")).map_err(|e| e.to_string())?;
        let bin = self.0.with_file_name("bin");
        if !bin.exists() {
            return std::fs::rename(&self.0, &bin).map_err(|e| e.to_string());
        }
        #[cfg(windows)]
        return swap_files(&self.0, &bin);
        #[cfg(unix)]
        if !exchange(&self.0, &bin) {
            // A file system without an atomic exchange: two renames, a moment apart.
            let old = self.0.with_file_name(format!("bin.{}.old", std::process::id()));
            std::fs::rename(&bin, &old).map_err(|e| e.to_string())?;
            std::fs::rename(&self.0, &bin).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_dir_all(old);
        }
        #[cfg(unix)]
        Ok(())
    }
}

/// Windows renames a running exe aside but cannot replace it or its folder, so bin/ changes file by file: each old file
/// goes to `<name>.old-<pid>-<nanos>`, the new one takes its name, and .version goes in last so a half-done update is
/// redone at the next check. Files only: a release holds binaries, no folders.
#[cfg(windows)]
fn swap_files(staging: &Path, bin: &Path) -> Result<(), String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(staging).map_err(|e| e.to_string())?.flatten() {
        names.push(entry.file_name());
    }
    let keep: std::collections::HashSet<_> = names.iter().cloned().collect();
    for entry in std::fs::read_dir(bin).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name();
        if !keep.contains(&name) && !is_aside(&name) {
            set_aside(&entry.path())?;
        }
    }
    names.sort_by_key(|name| name == ".version");
    for name in names {
        let target = bin.join(&name);
        let aside = if target.exists() { Some(set_aside(&target)?) } else { None };
        if let Err(e) = std::fs::rename(staging.join(&name), &target) {
            if let Some(aside) = aside {
                let _ = std::fs::rename(aside, &target);
            }
            return Err(e.to_string());
        }
    }
    remove_aside(bin);
    Ok(())
}

#[cfg(windows)]
fn is_aside(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().contains(".old-")
}

/// Renames a file to a name of its own beside it, which works while it runs.
#[cfg(windows)]
fn set_aside(path: &Path) -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".old-{}-{nanos}", std::process::id()));
    let aside = PathBuf::from(aside);
    std::fs::rename(path, &aside).map_err(|e| format!("cannot move {} aside: {e}", path.display()))?;
    Ok(aside)
}

/// Deletes the files set aside in `bin`; one still running stays for the next try.
#[cfg(windows)]
pub(crate) fn remove_aside(bin: &Path) {
    let Ok(entries) = std::fs::read_dir(bin) else { return };
    for entry in entries.flatten() {
        if is_aside(&entry.file_name()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Swaps two paths in one step.
#[cfg(unix)]
fn exchange(a: &Path, b: &Path) -> bool {
    let (Ok(a), Ok(b)) = (CString::new(a.as_os_str().as_bytes()), CString::new(b.as_os_str().as_bytes())) else { return false };
    #[cfg(target_os = "macos")]
    // SAFETY: both are valid NUL-terminated paths that outlive the call.
    let done = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
    #[cfg(target_os = "linux")]
    // SAFETY: both are valid NUL-terminated paths that outlive the call; AT_FDCWD resolves them as given.
    let done = unsafe { libc::renameat2(libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_EXCHANGE) };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let done = -1;
    done == 0
}
