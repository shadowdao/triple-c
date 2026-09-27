//! Test-only helpers: throwaway git repositories built with the `git` CLI, so
//! marketplace code is exercised against real git objects over `file://`.
//! The git plumbing itself lives in [`super::git::test_support`] (one copy).

use std::fs;

use super::git::test_support::{file_url, git, git_available};

pub struct GitFixture {
    pub dir: tempfile::TempDir,
}

impl GitFixture {
    /// `None` (with a note on stderr) when `git` is not installed; callers skip.
    pub fn new() -> Option<Self> {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return None;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        Some(Self { dir })
    }

    pub fn url(&self) -> String {
        file_url(self.dir.path())
    }

    pub fn write(&self, path: &str, contents: &str) -> &Self {
        let p = self.dir.path().join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, contents).unwrap();
        self
    }

    pub fn write_exec(&self, path: &str, contents: &str) -> &Self {
        self.write(path, contents);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = self.dir.path().join(path);
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        self
    }

    /// Commit everything and return the new commit id (40 hex).
    pub fn commit(&self, message: &str) -> String {
        git(self.dir.path(), &["add", "-A"]);
        git(
            self.dir.path(),
            &["commit", "-q", "--allow-empty", "-m", message],
        );
        git(self.dir.path(), &["rev-parse", "HEAD"])
    }

    /// A repo with one item of every kind, committed. Returns the commit.
    pub fn with_all_kinds(&self) -> String {
        self.write(
            "agents/code-reviewer.md",
            "---\nname: code-reviewer\ndescription: Reviews code\n---\nReview the diff.\n",
        )
        .write(
            "skills/example-skill/SKILL.md",
            "---\nname: example-skill\ndescription: An example skill\n---\nDo the thing.\n",
        )
        .write(
            "commands/example-command.md",
            "---\ndescription: An example command\n---\nRun the example.\n",
        )
        .write(
            "hooks/notify-on-stop/hook.json",
            r#"{"name":"notify-on-stop","description":"Ping on stop","hooks":{"Stop":[{"hooks":[{"type":"command","command":"${HOOK_DIR}/notify.sh"}]}]}}"#,
        )
        .write_exec("hooks/notify-on-stop/notify.sh", "#!/bin/sh\necho done\n")
        .write(
            "plugins/.claude-plugin/marketplace.json",
            r#"{"name":"upstream","owner":{"name":"Test"},"plugins":[{"name":"example-plugin","source":"./example-plugin","description":"An example plugin"}]}"#,
        )
        .write(
            "plugins/example-plugin/.claude-plugin/plugin.json",
            r#"{"name":"example-plugin","version":"0.1.0"}"#,
        )
        .write(
            "plugins/example-plugin/skills/hello/SKILL.md",
            "---\nname: hello\ndescription: Says hello\n---\nSay hello.\n",
        );
        self.commit("all kinds")
    }
}
