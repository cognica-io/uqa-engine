//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Directory notifications plus the mount namespace's pollable change counter.

use std::{
    ffi::CString,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

pub(super) struct Watch {
    notifications: OwnedFd,
    mounts: File,
}

impl Watch {
    pub(super) fn new(parent: &Path) -> io::Result<Self> {
        // Register mount observation before resolving any watched pathname.
        let mounts = File::open("/proc/self/mounts")?;
        // SAFETY: inotify_init1 returns a new descriptor, owned immediately.
        let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let notifications = unsafe { OwnedFd::from_raw_fd(descriptor) };
        for directory in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let path = directory;
            if !path.symlink_metadata()?.file_type().is_dir() {
                return Err(super::unsupported());
            }
            let path =
                CString::new(path.as_os_str().as_bytes()).map_err(|_| super::unsupported())?;
            let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: the path is NUL-terminated and the output is correctly sized.
            if unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let filesystem = unsafe { filesystem.assume_init() };
            // Known local filesystems. Network, FUSE and overlay filesystems can
            // change behind fsnotify; they retain direct identity checks.
            if !matches!(
                filesystem.f_type as u64,
                0xef53 | 0x5846_5342 | 0x9123_683e | 0x0102_1994 | 0x8584_58f6
            ) {
                return Err(super::unsupported());
            }
            let mask = libc::IN_ONLYDIR
                | libc::IN_DONT_FOLLOW
                | libc::IN_MOVE_SELF
                | libc::IN_DELETE_SELF
                | libc::IN_ATTRIB
                | if directory == parent {
                    libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO
                } else {
                    0
                };
            // SAFETY: the descriptor and NUL-terminated directory path are live.
            if unsafe { libc::inotify_add_watch(notifications.as_raw_fd(), path.as_ptr(), mask) }
                < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            notifications,
            mounts,
        })
    }

    pub(super) fn changed(&self) -> io::Result<bool> {
        let mut events = [
            libc::pollfd {
                fd: self.notifications.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.mounts.as_raw_fd(),
                events: libc::POLLPRI,
                revents: 0,
            },
        ];
        // SAFETY: poll borrows two initialized descriptors and waits zero ms.
        let ready = unsafe { libc::poll(events.as_mut_ptr(), events.len() as libc::nfds_t, 0) };
        // Any event, including overflow, unmount, ignored watches and fd errors,
        // invalidates the entire one-shot watch without interpreting filenames.
        if ready < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ready != 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_event_invalidates_the_mount_and_directory_poll() {
        let directory = tempfile::tempdir().unwrap();
        let path = CString::new(directory.path().as_os_str().as_bytes()).unwrap();
        let descriptor = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        assert!(descriptor >= 0);
        let notifications = unsafe { OwnedFd::from_raw_fd(descriptor) };
        assert!(
            unsafe {
                libc::inotify_add_watch(
                    notifications.as_raw_fd(),
                    path.as_ptr(),
                    libc::IN_CREATE | libc::IN_DELETE,
                )
            } >= 0
        );
        let watch = Watch {
            notifications,
            mounts: File::open("/proc/self/mounts").unwrap(),
        };
        assert!(!watch.changed().unwrap());
        std::fs::write(directory.path().join("new"), b"value").unwrap();
        assert!(watch.changed().unwrap());
    }
}
