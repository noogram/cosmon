// SPDX-License-Identifier: AGPL-3.0-only

//! Platform process-identity witnesses for infrastructure adapters.
//!
//! A numeric PID is reusable by the operating system and therefore cannot,
//! alone, authenticate a previously launched worker. This crate exposes the
//! opaque launch-time token that adapters persist beside a PID and compare
//! before treating that PID as the same worker.

/// Return the opaque launch-time token for `pid` on the current platform.
///
/// The token is stable for one process incarnation and changes when the kernel
/// reuses the numeric PID for another process. It is deliberately not a wall
/// clock: callers must only persist and compare it on the same host/platform.
/// `None` means the host could not establish a witness, including when the PID
/// no longer exists.
#[must_use]
pub fn process_start_time(pid: u32) -> Option<u64> {
    let pid = i32::try_from(pid).ok()?;

    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let closing_paren = stat.rfind(')')?;
        stat.get(closing_paren + 2..)?
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()
    }

    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let expected_size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `info` is valid writable storage for the `PROC_PIDTBSDINFO`
        // result, and `pid` plus `expected_size` both fit the C ABI types.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                expected_size,
            )
        };
        if written != expected_size {
            return None;
        }
        // SAFETY: a complete `proc_bsdinfo` result was reported above.
        let info = unsafe { info.assume_init() };
        info.pbi_start_tvsec
            .checked_mul(1_000_000)
            .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}
