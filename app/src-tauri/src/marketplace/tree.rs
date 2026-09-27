//! A read-only view of a repository tree at one commit.
//!
//! The catalog parser only ever talks to [`TreeView`], so it is tested
//! against [`MemTree`] with no git involved, and runs in production against
//! [`GitTree`], which reads git objects straight out of the bare cache.

#[cfg(test)]
use std::collections::BTreeMap;

#[cfg(test)]
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    /// Anything else git can hold (submodule commits). Never installable.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub executable: bool,
}

pub trait TreeView {
    /// Entries of the directory at `path` (`""` = root). `Ok(None)` if absent or not a dir.
    fn list_dir(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String>;
    /// Contents of the regular file at `path`, if it is at most `max_bytes`.
    /// `Ok(None)` if absent or not a file. A larger file is
    /// [`ReadError::TooLarge`], decided before its contents are loaded.
    fn read_file(&self, path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, ReadError>;
    /// Stable content id of the entry at `path`; `None` if absent.
    fn entry_id(&self, path: &str) -> Result<Option<String>, String>;
}

/// Why [`TreeView::read_file`] returned no contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The file is larger than the caller's cap (known from the object
    /// header, so nothing was inflated).
    TooLarge {
        path: String,
        max_bytes: u64,
    },
    Other(String),
}

/// `"2 MiB"`, `"64 KiB"` or `"N bytes"`.
pub(crate) fn describe_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes.is_multiple_of(KIB) {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{} bytes", bytes)
    }
}

impl From<ReadError> for String {
    fn from(e: ReadError) -> String {
        match e {
            ReadError::TooLarge { path, max_bytes } => {
                format!("{} is larger than {}", path, describe_size(max_bytes))
            }
            ReadError::Other(msg) => msg,
        }
    }
}

impl From<String> for ReadError {
    fn from(msg: String) -> Self {
        ReadError::Other(msg)
    }
}

/// Hex-encode `bytes`. Shared by [`MemTree`]'s content id (test-only) and
/// `catalog::item_fingerprint`'s plugin-entry hash (production), so there is
/// one hex formatter rather than two copies of the same `format!("{:02x}")`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// A tree at one commit of a bare gix repository.
pub struct GitTree {
    repo: gix::Repository,
    tree_id: gix::ObjectId,
}

#[cfg(test)]
thread_local! {
    static REPO_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has opened a cache repo (tests only).
#[cfg(test)]
pub fn repo_opens() -> usize {
    REPO_OPENS.with(|c| c.get())
}

/// Open a bare cache. Several [`GitTree`]s can share one open repo through
/// [`GitTree::at`] (cloning a `gix::Repository` shares its object store).
pub fn open_repo(repo_path: &std::path::Path) -> Result<gix::Repository, String> {
    #[cfg(test)]
    REPO_OPENS.with(|c| c.set(c.get() + 1));
    gix::open(repo_path).map_err(|e| format!("Could not open the marketplace cache: {}", e))
}

impl GitTree {
    pub fn open(repo_path: &std::path::Path, commit: &str) -> Result<Self, String> {
        Self::at(open_repo(repo_path)?, commit)
    }

    /// The tree at `commit` of an already open repo.
    pub fn at(repo: gix::Repository, commit: &str) -> Result<Self, String> {
        let oid = gix::ObjectId::from_hex(commit.as_bytes())
            .map_err(|e| format!("Invalid commit id {}: {}", commit, e))?;
        let tree_id = repo
            .find_commit(oid)
            .map_err(|e| format!("Commit {} is not in the marketplace cache: {}", commit, e))?
            .tree_id()
            .map_err(|e| format!("Commit {} has no tree: {}", commit, e))?
            .detach();
        Ok(Self { repo, tree_id })
    }

    fn root(&self) -> Result<gix::Tree<'_>, String> {
        self.repo
            .find_tree(self.tree_id)
            .map_err(|e| format!("Could not read tree {}: {}", self.tree_id, e))
    }

    /// `(object id, mode)` of the entry at `path`, or `None`.
    fn lookup(
        &self,
        path: &str,
    ) -> Result<Option<(gix::ObjectId, gix::object::tree::EntryMode)>, String> {
        if path.is_empty() {
            return Ok(Some((
                self.tree_id,
                gix::object::tree::EntryKind::Tree.into(),
            )));
        }
        let root = self.root()?;
        let entry = root
            .lookup_entry_by_path(path)
            .map_err(|e| format!("Could not look up {}: {}", path, e))?;
        Ok(entry.map(|e| (e.object_id(), e.mode())))
    }
}

impl TreeView for GitTree {
    fn list_dir(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        let Some((id, mode)) = self.lookup(path)? else {
            return Ok(None);
        };
        if !mode.is_tree() {
            return Ok(None);
        }
        let tree = self
            .repo
            .find_tree(id)
            .map_err(|e| format!("Could not read {}: {}", path, e))?;
        let mut out = Vec::new();
        for entry in tree.iter() {
            let entry = entry.map_err(|e| format!("Could not read {}: {:?}", path, e))?;
            let mode = entry.mode();
            let kind = if mode.is_tree() {
                EntryKind::Dir
            } else if mode.is_link() {
                EntryKind::Symlink
            } else if mode.is_blob() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            out.push(DirEntry {
                name: entry.filename().to_string(),
                kind,
                executable: mode.is_executable(),
            });
        }
        Ok(Some(out))
    }

    fn read_file(&self, path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, ReadError> {
        let Some((id, mode)) = self.lookup(path)? else {
            return Ok(None);
        };
        if !mode.is_blob() {
            return Ok(None);
        }
        // The header alone gives the size; a blob over the cap is never
        // inflated (a compressible multi-GB file would otherwise be).
        let size = self
            .repo
            .find_header(id)
            .map_err(|e| format!("Could not read {}: {}", path, e))?
            .size();
        if size > max_bytes {
            return Err(ReadError::TooLarge {
                path: path.to_string(),
                max_bytes,
            });
        }
        let mut blob = self
            .repo
            .find_blob(id)
            .map_err(|e| format!("Could not read {}: {}", path, e))?;
        Ok(Some(blob.take_data()))
    }

    fn entry_id(&self, path: &str) -> Result<Option<String>, String> {
        Ok(self.lookup(path)?.map(|(id, _)| id.to_string()))
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
enum MemNode {
    File { data: Vec<u8>, executable: bool },
    Symlink { target: String },
}

/// In-memory tree for tests: path → node. Directories are implied by paths.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct MemTree {
    nodes: BTreeMap<String, MemNode>,
}

#[cfg(test)]
impl MemTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn file(mut self, path: &str, contents: &str) -> Self {
        self.nodes.insert(
            path.to_string(),
            MemNode::File {
                data: contents.as_bytes().to_vec(),
                executable: false,
            },
        );
        self
    }

    pub fn exec_file(mut self, path: &str, contents: &str) -> Self {
        self.nodes.insert(
            path.to_string(),
            MemNode::File {
                data: contents.as_bytes().to_vec(),
                executable: true,
            },
        );
        self
    }

    pub fn symlink(mut self, path: &str, target: &str) -> Self {
        self.nodes.insert(
            path.to_string(),
            MemNode::Symlink {
                target: target.to_string(),
            },
        );
        self
    }

    /// Place a file so that, inside `dir`, it is listed under the literal
    /// entry name `name` — including a name `file`/`exec_file`/`symlink`
    /// could never be asked to produce because it doesn't correspond to any
    /// real filesystem path a caller here would construct: `.`, `..`, empty,
    /// or containing `/`, `\` or a NUL byte. Exists only so a test can drive
    /// `catalog::collect_dir`'s hostile-entry-name rejection without relying
    /// on incidental behaviour of path-string splitting.
    pub fn raw_named_file(mut self, dir: &str, name: &str, contents: &str) -> Self {
        let path = if dir.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", dir, name)
        };
        self.nodes.insert(
            path,
            MemNode::File {
                data: contents.as_bytes().to_vec(),
                executable: false,
            },
        );
        self
    }

    fn is_dir(&self, path: &str) -> bool {
        if path.is_empty() {
            return true;
        }
        let prefix = format!("{}/", path);
        self.nodes.keys().any(|k| k.starts_with(&prefix))
    }
}

#[cfg(test)]
impl TreeView for MemTree {
    fn list_dir(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        if self.nodes.contains_key(path) || !self.is_dir(path) {
            return Ok(None);
        }
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{}/", path)
        };
        let mut out: BTreeMap<String, DirEntry> = BTreeMap::new();
        for (key, node) in &self.nodes {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((dir, _)) => {
                    out.entry(dir.to_string()).or_insert(DirEntry {
                        name: dir.to_string(),
                        kind: EntryKind::Dir,
                        executable: false,
                    });
                }
                None => {
                    let (kind, executable) = match node {
                        MemNode::File { executable, .. } => (EntryKind::File, *executable),
                        MemNode::Symlink { .. } => (EntryKind::Symlink, false),
                    };
                    out.insert(
                        rest.to_string(),
                        DirEntry {
                            name: rest.to_string(),
                            kind,
                            executable,
                        },
                    );
                }
            }
        }
        Ok(Some(out.into_values().collect()))
    }

    fn read_file(&self, path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, ReadError> {
        match self.nodes.get(path) {
            Some(MemNode::File { data, .. }) if data.len() as u64 > max_bytes => {
                Err(ReadError::TooLarge {
                    path: path.to_string(),
                    max_bytes,
                })
            }
            Some(MemNode::File { data, .. }) => Ok(Some(data.clone())),
            _ => Ok(None),
        }
    }

    fn entry_id(&self, path: &str) -> Result<Option<String>, String> {
        let mut hasher = Sha256::new();
        let mut found = false;
        let prefix = format!("{}/", path);
        for (key, node) in &self.nodes {
            if key != path && !key.starts_with(&prefix) {
                continue;
            }
            found = true;
            hasher.update(key.as_bytes());
            hasher.update([0]);
            match node {
                MemNode::File { data, executable } => {
                    hasher.update([if *executable { b'x' } else { b'f' }]);
                    hasher.update(data);
                }
                MemNode::Symlink { target } => {
                    hasher.update(b"l");
                    hasher.update(target.as_bytes());
                }
            }
            hasher.update([0]);
        }
        Ok(found.then(|| hex(&hasher.finalize())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_tree_lists_files_dirs_and_symlinks() {
        let t = MemTree::new()
            .file("agents/a.md", "x")
            .exec_file("hooks/h/run.sh", "#!/bin/sh")
            .symlink("agents/link.md", "a.md");
        let root = t.list_dir("").unwrap().unwrap();
        assert_eq!(
            root.iter()
                .map(|e| (e.name.as_str(), e.kind))
                .collect::<Vec<_>>(),
            vec![("agents", EntryKind::Dir), ("hooks", EntryKind::Dir)]
        );
        let agents = t.list_dir("agents").unwrap().unwrap();
        assert_eq!(agents[1].kind, EntryKind::Symlink);
        let hook = t.list_dir("hooks/h").unwrap().unwrap();
        assert!(hook[0].executable);
        assert_eq!(t.list_dir("agents/a.md").unwrap(), None);
        assert_eq!(t.list_dir("missing").unwrap(), None);
        assert_eq!(t.read_file("agents/a.md", 10).unwrap().unwrap(), b"x");
        assert_eq!(t.read_file("agents", 10).unwrap(), None);
    }

    #[test]
    fn mem_tree_entry_id_changes_only_with_content() {
        let a = MemTree::new()
            .file("skills/s/SKILL.md", "one")
            .file("agents/x.md", "x");
        let b = MemTree::new()
            .file("skills/s/SKILL.md", "one")
            .file("agents/x.md", "changed");
        let c = MemTree::new()
            .file("skills/s/SKILL.md", "two")
            .file("agents/x.md", "x");
        assert_eq!(
            a.entry_id("skills/s").unwrap(),
            b.entry_id("skills/s").unwrap()
        );
        assert_ne!(
            a.entry_id("skills/s").unwrap(),
            c.entry_id("skills/s").unwrap()
        );
        assert_eq!(a.entry_id("nope").unwrap(), None);
    }

    /// Review #9: the size comes from the object header, so a blob over the
    /// cap is refused without its body ever being inflated. The fixture's
    /// loose object is cut short after its header: reading the body would
    /// fail, while the header still names the full size.
    #[test]
    fn git_tree_refuses_an_oversized_blob_from_its_header() {
        use crate::marketplace::git::test_support::{git, git_available, init_repo};
        if !git_available() {
            return;
        }
        const CAP: u64 = 64 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(CAP as usize + 1);
        let commit = init_repo(
            dir.path(),
            &[
                ("agents/big.md", &big, false),
                ("agents/small.md", "hi", false),
            ],
        );
        let blob = git(dir.path(), &["rev-parse", "HEAD:agents/big.md"]);
        let loose = dir
            .path()
            .join(".git/objects")
            .join(&blob[..2])
            .join(&blob[2..]);
        let bytes = std::fs::read(&loose).unwrap();
        let mut perms = std::fs::metadata(&loose).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false); // git writes objects read-only
        std::fs::set_permissions(&loose, perms).unwrap();
        std::fs::write(&loose, &bytes[..40.min(bytes.len())]).unwrap();

        let tree = GitTree::open(&dir.path().join(".git"), &commit).unwrap();
        let err = String::from(tree.read_file("agents/big.md", CAP).unwrap_err());
        assert!(err.contains("larger than 64 KiB"), "{err}");
        // The body really is unreadable: under a cap it fits, the read fails
        // for another reason — so the refusal above never inflated it.
        let body = String::from(tree.read_file("agents/big.md", 2 * CAP).unwrap_err());
        assert!(!body.contains("larger than"), "{body}");
        assert_eq!(
            tree.read_file("agents/small.md", CAP).unwrap().unwrap(),
            b"hi"
        );
        assert_eq!(tree.read_file("agents/missing.md", CAP).unwrap(), None);
    }

    #[test]
    fn trees_at_several_commits_share_one_open_repo() {
        use crate::marketplace::git::test_support::{commit_files, git_available, init_repo};
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let c1 = init_repo(dir.path(), &[("a.md", "one", false)]);
        let c2 = commit_files(dir.path(), &[("a.md", "two", false)], "second");
        let before = repo_opens();
        let repo = open_repo(&dir.path().join(".git")).unwrap();
        let t1 = GitTree::at(repo.clone(), &c1).unwrap();
        let t2 = GitTree::at(repo, &c2).unwrap();
        assert_eq!(repo_opens() - before, 1);
        assert_eq!(t1.read_file("a.md", 10).unwrap().unwrap(), b"one");
        assert_eq!(t2.read_file("a.md", 10).unwrap().unwrap(), b"two");
    }

    #[test]
    fn mem_tree_applies_the_same_cap() {
        let t = MemTree::new().file("a.md", "12345");
        assert_eq!(t.read_file("a.md", 5).unwrap().unwrap(), b"12345");
        let err = String::from(t.read_file("a.md", 4).unwrap_err());
        assert!(err.contains("a.md is larger than 4 bytes"), "{err}");
    }
}
