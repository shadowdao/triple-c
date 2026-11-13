//! Pseudo-terminals for `tty: true` execs.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use tokio::io::unix::AsyncFd;

/// The agent's end of a pty. Shared (via `Arc`) by the output pump, the stdin
/// writer and the exec registry, which needs it for `resize`.
pub struct PtyMaster {
    fd: AsyncFd<OwnedFd>,
}

/// Open a pty sized `cols`×`rows`, returning the master and the slave the
/// child gets as stdin/stdout/stderr.
///
/// Both ends are opened `O_CLOEXEC` *atomically* (`posix_openpt`, then `open`
/// on the slave path) rather than with `openpty` and a later `FD_CLOEXEC`:
/// execs are spawned concurrently from other threads, and a master fd leaked
/// into an unrelated child would keep this terminal open after its own process
/// exits — the attach would never see EOF.
pub fn open(cols: u16, rows: u16) -> io::Result<(PtyMaster, OwnedFd)> {
    unsafe {
        let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if m < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(m);
        if libc::grantpt(m) != 0 || libc::unlockpt(m) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = [0 as libc::c_char; 128];
        let rc = libc::ptsname_r(m, name.as_mut_ptr(), name.len());
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        let s = libc::open(
            CStr::from_ptr(name.as_ptr()).as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        if s < 0 {
            return Err(io::Error::last_os_error());
        }
        let slave = OwnedFd::from_raw_fd(s);
        set_size(m, cols, rows)?;
        let flags = libc::fcntl(m, libc::F_GETFL);
        if flags < 0 || libc::fcntl(m, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
        // Sound: `master` is an `OwnedFd`, open for as long as the AsyncFd
        // owns it, and `as_raw_fd` always returns that same descriptor.
        Ok((PtyMaster { fd: AsyncFd::register(master)? }, slave))
    }
}

fn set_size(fd: libc::c_int, cols: u16, rows: u16) -> io::Result<()> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl PtyMaster {
    /// `TIOCSWINSZ` on the master; the kernel delivers `SIGWINCH` to the
    /// terminal's foreground process group, as Docker's resize does.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        set_size(self.fd.get_ref().as_raw_fd(), cols, rows)
    }

    /// Read output. `Ok(0)` is end of stream: Linux reports "every slave fd is
    /// closed" on the master as `EIO`, which is folded into EOF here.
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            let res = guard.try_io(|inner| {
                let n = unsafe {
                    libc::read(inner.get_ref().as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match res {
                Ok(Ok(n)) => return Ok(n),
                Ok(Err(e)) if e.raw_os_error() == Some(libc::EIO) => return Ok(0),
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }

    pub async fn write_all(&self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let mut guard = self.fd.writable().await?;
            let res = guard.try_io(|inner| {
                let n = unsafe {
                    libc::write(inner.get_ref().as_raw_fd(), data.as_ptr().cast(), data.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match res {
                Ok(Ok(n)) => data = &data[n..],
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }
}
