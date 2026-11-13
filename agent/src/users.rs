//! User resolution through `/etc/passwd` and `/etc/group`.
//!
//! The static musl build has no NSS, so `getpwnam_r` and `getgrouplist` read
//! the container's files directly — which is exactly the database `docker exec
//! --user` resolves against (runc parses `/etc/passwd` itself), so the two
//! cannot disagree about who a name is.

use std::ffi::{CStr, CString};

#[derive(Debug, Clone)]
pub struct UserInfo {
    pub name: String,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub home: String,
    /// Supplementary groups, primary included — what `initgroups` would set.
    /// Resolved here, in the parent, because `pre_exec` runs between `fork`
    /// and `exec` in a multi-threaded process where reading files and
    /// allocating are not safe; the child only calls `setgroups` on this list.
    pub groups: Vec<libc::gid_t>,
}

/// Resolve a user name (or, as Docker also allows, a numeric uid).
///
/// A numeric uid with no passwd entry still resolves, with gid 0 and HOME `/`,
/// because that is what runc does for `--user 1234` in an image without that
/// user.
pub fn lookup(name: &str) -> Option<UserInfo> {
    let cname = CString::new(name).ok()?;
    if let Some(u) = getpw(|pwd, buf, len, res| unsafe {
        libc::getpwnam_r(cname.as_ptr(), pwd, buf, len, res)
    }) {
        return Some(u);
    }
    let uid: libc::uid_t = name.parse().ok()?;
    if let Some(u) = getpw(|pwd, buf, len, res| unsafe { libc::getpwuid_r(uid, pwd, buf, len, res) }) {
        return Some(u);
    }
    Some(UserInfo {
        name: name.to_string(),
        uid,
        gid: 0,
        home: "/".to_string(),
        groups: vec![0],
    })
}

/// The user the agent itself runs as.
pub fn current() -> Option<UserInfo> {
    let uid = unsafe { libc::geteuid() };
    getpw(|pwd, buf, len, res| unsafe { libc::getpwuid_r(uid, pwd, buf, len, res) })
}

fn getpw(
    call: impl Fn(*mut libc::passwd, *mut libc::c_char, libc::size_t, *mut *mut libc::passwd) -> libc::c_int,
) -> Option<UserInfo> {
    let mut size = 1024usize;
    loop {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buf = vec![0 as libc::c_char; size];
        let mut res: *mut libc::passwd = std::ptr::null_mut();
        let rc = call(&mut pwd, buf.as_mut_ptr(), buf.len(), &mut res);
        if rc == libc::ERANGE && size < (1 << 20) {
            size *= 4;
            continue;
        }
        if rc != 0 || res.is_null() {
            return None;
        }
        let name = unsafe { CStr::from_ptr(pwd.pw_name) }.to_string_lossy().into_owned();
        let home = unsafe { CStr::from_ptr(pwd.pw_dir) }.to_string_lossy().into_owned();
        let groups = group_list(&name, pwd.pw_gid);
        return Some(UserInfo {
            name,
            uid: pwd.pw_uid,
            gid: pwd.pw_gid,
            home,
            groups,
        });
    }
}

fn group_list(name: &str, gid: libc::gid_t) -> Vec<libc::gid_t> {
    let Ok(cname) = CString::new(name) else {
        return vec![gid];
    };
    let mut n: libc::c_int = 32;
    loop {
        let mut groups = vec![0 as libc::gid_t; n as usize];
        let mut count = n;
        let rc = unsafe { libc::getgrouplist(cname.as_ptr(), gid, groups.as_mut_ptr(), &mut count) };
        if rc >= 0 {
            groups.truncate(count.max(0) as usize);
            return groups;
        }
        // -1 means "too small" and `count` now holds the size needed (glibc
        // and musl both). Guard against a libc that does not update it.
        if count <= n {
            if n >= 65536 {
                return vec![gid];
            }
            n *= 2;
        } else {
            n = count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_current_user_by_name_and_uid() {
        let me = current().expect("current user has a passwd entry");
        let by_name = lookup(&me.name).unwrap();
        assert_eq!(by_name.uid, me.uid);
        assert!(by_name.groups.contains(&me.gid));
        let by_uid = lookup(&me.uid.to_string()).unwrap();
        assert_eq!(by_uid.name, me.name);
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert!(lookup("no-such-user-triple-c").is_none());
        assert!(lookup("bad\0name").is_none());
        // A bare uid nobody owns still resolves, as runc does.
        let u = lookup("4000123").unwrap();
        assert_eq!((u.uid, u.gid, u.home.as_str()), (4000123, 0, "/"));
    }
}
