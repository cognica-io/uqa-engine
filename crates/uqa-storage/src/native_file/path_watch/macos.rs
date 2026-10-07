//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vnode and mount notifications on local Apple filesystems.

use std::{
    fs::{File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
};

pub(super) struct Watch {
    queue: OwnedFd,
    _directories: Vec<File>,
}

impl Watch {
    pub(super) fn new(parent: &Path) -> io::Result<Self> {
        // SAFETY: kqueue creates a fresh descriptor; it is owned immediately.
        let descriptor = unsafe { libc::kqueue() };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
        // SAFETY: F_SETFD borrows our valid descriptor and an integer flag.
        if unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // VQ_MOUNT, VQ_UNMOUNT and VQ_UPDATE from sys/mount.h cover changes
        // that do not rename a watched vnode (notably mounting over a path).
        register(&queue, 0, libc::EVFILT_FS, 0x0008 | 0x0010 | 0x0100)?;
        let mut directories = Vec::new();
        // Observe ancestors before resolving descendants so a concurrent
        // rename cannot leave a watch attached to an abandoned path prefix.
        for path in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if !path.symlink_metadata()?.file_type().is_dir() {
                return Err(super::unsupported());
            }
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_EVTONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?;
            // SAFETY: fstatfs fills a correctly sized output through a live fd.
            let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
            if unsafe { libc::fstatfs(directory.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let filesystem = unsafe { filesystem.assume_init() };
            let name: Vec<_> = filesystem
                .f_fstypename
                .iter()
                .take_while(|byte| **byte != 0)
                .map(|byte| *byte as u8)
                .collect();
            if filesystem.f_flags & libc::MNT_LOCAL as u32 == 0
                || !matches!(name.as_slice(), b"apfs" | b"hfs")
            {
                return Err(super::unsupported());
            }
            let flags = libc::NOTE_RENAME
                | libc::NOTE_DELETE
                | libc::NOTE_REVOKE
                | libc::NOTE_ATTRIB
                | if path == parent {
                    libc::NOTE_WRITE | libc::NOTE_LINK
                } else {
                    0
                };
            register(
                &queue,
                directory.as_raw_fd() as usize,
                libc::EVFILT_VNODE,
                flags,
            )?;
            directories.push(directory);
        }
        Ok(Self {
            queue,
            _directories: directories,
        })
    }

    pub(super) fn changed(&self) -> io::Result<bool> {
        let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: the queue is live, no input events are supplied, and the one
        // output slot and zero timeout are valid for the complete call.
        let count = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                event.as_mut_ptr(),
                1,
                &raw const timeout,
            )
        };
        if count < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(count != 0)
        }
    }
}

fn register(queue: &OwnedFd, identity: usize, filter: i16, flags: u32) -> io::Result<()> {
    let event = libc::kevent {
        ident: identity,
        filter,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: flags,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: the live queue borrows exactly one initialized input event. With
    // no event output, registration errors are returned directly.
    let result = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &raw const event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
