use super::*;

const KEY: &str = "0b9c6f7e-1d2a-4c3b-9e8f-7a6b5c4d3e2f";

fn claude_inner() -> Vec<String> {
    vec!["bash".into(), "-c".into(), "flock … claude update\nexec claude '-n' 'x'\n".into()]
}

#[test]
fn a_new_tab_runs_its_unchanged_command_as_the_only_window_of_its_own_server() {
    let cmd = new_session_cmd(KEY, TerminalKind::Claude, Some("main"), claude_inner()).unwrap();
    assert_eq!(
        &cmd[..10],
        &[
            "tmux", "-L", &format!("triple-c-{KEY}"), "-f", "/etc/triple-c/tmux.conf",
            "new-session", "-A", "-s", "claude", "--",
        ]
        .map(String::from)
    );
    // The inner command is passed through as argv, untouched.
    assert_eq!(&cmd[10..13], claude_inner().as_slice());
    assert_eq!(&cmd[13..], &[";", "set-option", "-q", "@triple_c_name", "main"].map(String::from));
    // Bash tabs say so in the session name; no label, no second command.
    let bash = new_session_cmd(KEY, TerminalKind::Bash, None, vec!["bash".into(), "-l".into()]).unwrap();
    assert_eq!(bash[8], "bash");
    assert_eq!(bash.last().unwrap(), "-l");
    assert!(!bash.contains(&";".to_string()));
}

#[test]
fn keys_are_socket_safe_and_an_argument_ending_in_a_semicolon_is_refused() {
    for bad in ["", "../x", "a b", "a;b", "x/y", &"a".repeat(65)] {
        assert!(validate_key(bad).is_err(), "{bad:?}");
        assert!(new_session_cmd(bad, TerminalKind::Bash, None, vec!["bash".into()]).is_err());
        assert!(attach_cmd(bad).is_err() && kill_cmd(bad).is_err() && history_cmd(bad).is_err());
    }
    assert!(validate_key(KEY).is_ok());
    // tmux would run what follows as tmux commands.
    let e = new_session_cmd(KEY, TerminalKind::Bash, None, vec!["sh".into(), "-c".into(), "x;".into()]);
    assert!(e.is_err());
}

#[test]
fn a_label_is_cleaned_so_it_cannot_split_the_tmux_command_or_the_listing() {
    assert_eq!(clean_name(Some("Bob's tab")).as_deref(), Some("Bob's tab"));
    assert_eq!(clean_name(Some("a\tb\nc")).as_deref(), Some("abc"));
    assert_eq!(clean_name(Some("x;")).as_deref(), Some("x"));
    assert_eq!(clean_name(Some(r"x \;; ")).as_deref(), Some("x"));
    assert_eq!(clean_name(Some(" ;; ")), None);
    assert_eq!(clean_name(None), None);
    assert_eq!(clean_name(Some(&"é".repeat(200))).unwrap().chars().count(), 80);
    let cmd = new_session_cmd(KEY, TerminalKind::Claude, Some("evil;"), claude_inner()).unwrap();
    assert_eq!(cmd.last().unwrap(), "evil");
    assert!(cmd.iter().filter(|a| a.ends_with(';')).count() == 1, "only our separator: {cmd:?}");
}

#[test]
fn secrets_travel_in_the_exec_environment_never_on_argv() {
    let env = vec!["ANTHROPIC_API_KEY=sk-test-not-real".to_string(), "TERM=xterm".to_string()];
    let plan = plan_with(true, true, None, KEY, TerminalKind::Claude, None, claude_inner(), env).unwrap();
    assert!(plan.persistent);
    assert!(plan.cmd.iter().all(|a| !a.contains("sk-test-not-real")), "{:?}", plan.cmd);
    assert!(!plan.cmd.iter().any(|a| a == "-e"), "no new-session -e");
    // The server inherits the client's environment; the outer TERM is xterm.js's.
    assert_eq!(plan.env, vec!["ANTHROPIC_API_KEY=sk-test-not-real".to_string(), OUTER_TERM.to_string()]);
}

#[test]
fn docker_projects_and_images_without_tmux_launch_as_before() {
    let inner = claude_inner();
    let env = vec!["A=1".to_string()];
    // Docker: untouched, whatever the key looks like.
    let docker = plan_with(false, true, None, "not a key", TerminalKind::Claude, None, inner.clone(), env.clone()).unwrap();
    assert_eq!((docker.cmd, docker.env, docker.persistent, docker.notice), (inner.clone(), env.clone(), false, None));
    // AWS without tmux: the old launch, plus the notice when it is due.
    let old = plan_with(true, false, Some(NO_TMUX_NOTICE.into()), KEY, TerminalKind::Claude, None, inner.clone(), env.clone()).unwrap();
    assert_eq!((old.cmd, old.env, old.persistent), (inner, env, false));
    assert!(old.notice.unwrap().contains("no tmux"));
}

#[test]
fn the_no_tmux_notice_is_given_once_per_tunnel_and_never_for_an_unanswered_probe() {
    let id = format!("http://127.0.0.1:1/{}/", uuid::Uuid::new_v4());
    assert_eq!(take_notice(&id), None, "nothing probed: no notice");
    probes().lock().unwrap().insert(id.clone(), Probe { supported: false, noticed: false });
    assert!(take_notice(&id).is_some());
    assert_eq!(take_notice(&id), None, "once");
    // A new tunnel on the same port probes (and may notice) again.
    forget_probe(&id);
    assert!(probes().lock().unwrap().get(&id).is_none());
    probes().lock().unwrap().insert(id.clone(), Probe { supported: true, noticed: false });
    assert_eq!(take_notice(&id), None, "tmux present");
    forget_probe(&id);
}

#[test]
fn the_listing_parses_and_skips_what_is_not_ours() {
    let out = format!(
        "{KEY}\tclaude\t1794954824\t1\tmain tab\n\
         11111111-2222-3333-4444-555555555555\tbash\t1794954800\t0\t\n\
         bad key!\tbash\t1\t0\t\n\
         abc\tvim\t1\t0\t\n\
         abc\tbash\tnot-a-time\t0\t\n"
    );
    let got = parse_list(&out);
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0],
        RunningSession { key: KEY.into(), kind: TerminalKind::Claude, created_unix: 1794954824, attached: true, name: Some("main tab".into()) }
    );
    assert!(!got[1].attached && got[1].name.is_none() && got[1].kind == TerminalKind::Bash);
}

#[test]
fn the_workspace_scripts_only_touch_our_sockets() {
    for script in [list_cmd(), end_all_cmd()] {
        assert_eq!(&script[..2], &["sh".to_string(), "-c".to_string()]);
        assert!(script[2].contains("\"$d\"/triple-c-*"), "{}", script[2]);
        assert!(script[2].contains("${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)"));
    }
    let list = &list_cmd()[2];
    assert!(list.contains("display-message -p '#{session_name}\t#{session_created}\t#{session_attached}\t#{@triple_c_name}'"), "{list}");
    let end = &end_all_cmd()[2];
    assert!(end.contains("kill-server") && end.contains("pgrep -u \"$(id -u)\" -f '^tmux -L triple-c-'"));
    assert!(end.contains("[ $i -lt 50 ]"), "bounded wait");
    assert_eq!(probe_cmd()[2], "command -v tmux >/dev/null 2>&1 && [ -r /etc/triple-c/tmux.conf ]");
    // Reattach never creates; history is the part above the screen.
    assert_eq!(attach_cmd(KEY).unwrap()[5], "attach-session");
    assert!(!attach_cmd(KEY).unwrap().contains(&"-A".to_string()));
    let h = history_cmd(KEY).unwrap();
    assert!(h.ends_with(&["-S".into(), format!("-{HISTORY_LINES}"), "-E".into(), "-1".into()]));
}

#[test]
fn a_replay_pushes_the_whole_history_into_scrollback_before_tmux_draws() {
    assert!(replay_bytes("", 30).is_empty());
    assert!(replay_bytes("\n\n", 30).is_empty());
    let b = String::from_utf8(replay_bytes("one\n\x1b[31mtwo\n", 3)).unwrap();
    assert_eq!(b, "one\x1b[0m\r\n\x1b[31mtwo\x1b[0m\r\n\r\n\r\n\r\n");
}

/// The config tmux is started with, as the image ships it.
#[test]
fn the_image_ships_the_config_and_tmux() {
    let conf = include_str!("../../../../../container/triple-c-tmux.conf");
    for line in [
        "set -g prefix None",
        "unbind-key C-b",
        "set -g status off",
        "set -s escape-time 0",
        "set -g mouse off",
        "set -g remain-on-exit off",
        "set -g destroy-unattached off",
        "set -g window-size latest",
        "set -g default-terminal \"tmux-256color\"",
        r#"set -as terminal-overrides ",xterm*:smcup@:rmcup@:indn@:clear=\\E[H""#,
    ] {
        assert!(conf.lines().any(|l| l == line), "missing: {line}");
    }
    let df = include_str!("../../../../../container/Dockerfile");
    assert!(df.contains("COPY triple-c-tmux.conf /etc/triple-c/tmux.conf"));
    assert!(df.lines().any(|l| l.trim() == "tmux \\"), "tmux in an apt layer");
}
