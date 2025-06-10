//! A wake-up whenever the kernel changes links, addresses or routes.
//!
//! The messages are not parsed: the network service re-reads its whole
//! snapshot on any change, so all it needs is to know that one happened.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use tokio::io::unix::AsyncFd;

// From <linux/rtnetlink.h>; libc does not export all of them.
const RTMGRP_LINK: u32 = 1;
const RTMGRP_IPV4_IFADDR: u32 = 0x10;
const RTMGRP_IPV4_ROUTE: u32 = 0x40;
const RTMGRP_IPV6_IFADDR: u32 = 0x100;
const RTMGRP_IPV6_ROUTE: u32 = 0x400;

pub struct RouteEvents {
    fd: AsyncFd<OwnedFd>,
    buffer: Vec<u8>,
}

impl RouteEvents {
    pub fn new() -> io::Result<Self> {
        // SAFETY: plain socket creation; the fd is owned below.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a valid fd we just created and own.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        // SAFETY: sockaddr_nl is plain data; zeroed is a valid state.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address.nl_groups =
            RTMGRP_LINK | RTMGRP_IPV4_IFADDR | RTMGRP_IPV4_ROUTE | RTMGRP_IPV6_IFADDR | RTMGRP_IPV6_ROUTE;
        // SAFETY: the address struct and its length match.
        let bound = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &address as *const libc::sockaddr_nl as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bound < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            fd: AsyncFd::new(fd)?,
            buffer: vec![0; 8192],
        })
    }

    /// Wait for at least one change, then drain whatever else is queued so
    /// a burst of messages for one event wakes the caller once.
    pub async fn changed(&mut self) -> io::Result<()> {
        let mut received = false;
        loop {
            if received {
                // Already have one; only take what's immediately available.
                if self.recv()?.is_none() {
                    return Ok(());
                }
                continue;
            }
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|_| self.recv().map(|_| ())) {
                Ok(Ok(())) => received = true,
                Ok(Err(err)) => return Err(err),
                Err(_would_block) => continue,
            }
        }
    }

    /// One non-blocking read; `None` when nothing is queued.
    fn recv(&self) -> io::Result<Option<()>> {
        // SAFETY: the buffer is valid for its length; MSG_DONTWAIT keeps this non-blocking.
        let n = unsafe {
            libc::recv(
                self.fd.as_raw_fd(),
                self.buffer.as_ptr() as *mut libc::c_void,
                self.buffer.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if n >= 0 {
            return Ok(Some(()));
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::WouldBlock => Ok(None),
            _ => Err(err),
        }
    }
}
