use std::fs;
use std::path::PathBuf;

/// The level the dispatch is built with, and the level restored by hand if
/// installing it fails — see the failure branch in [`init`] for why that
/// matters more than it looks.
const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Info;

/// Returns the log directory path: `<data_dir>/triple-c/logs/`
fn log_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("triple-c").join("logs"))
}

/// Initialise logging to both stderr and a log file in the app data directory.
///
/// Logs are written to `<data_dir>/triple-c/logs/triple-c.log`.
/// A panic hook is also installed so that unexpected crashes are captured in the
/// same log file before the process exits.
pub fn init() {
    let log_file_path = log_dir().and_then(|dir| {
        fs::create_dir_all(&dir).ok()?;
        let path = dir.join("triple-c.log");
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(|file| (path, file))
    });

    let mut dispatch = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{} {} {}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.target(),
                message
            ))
        })
        .level(LOG_LEVEL)
        .chain(std::io::stderr());

    if let Some((_path, file)) = &log_file_path {
        dispatch = dispatch.chain(fern::Dispatch::new().chain(file.try_clone().unwrap()));
    }

    if let Err(e) = dispatch.apply() {
        // H2's other half. `fern::Dispatch::apply` calls `log::set_boxed_logger`
        // and only then `log::set_max_level`, so a failure returns with the
        // global filter still at its default, `LevelFilter::Off`. That is not
        // merely "no log output": every `log::info!(…)` expands to
        // `if Info <= max_level() { … }`, so at `Off` the macro never evaluates
        // its own arguments. Anything a call site put in an argument list —
        // a function call, an `await`, a side effect — silently stops
        // happening, app-wide, because a logger could not be installed.
        //
        // Call sites must not put effects in log arguments (see the
        // pre-migration scrub in `migration_commands.rs`), but "the whole
        // program's log macros are dead and nothing said so" is its own
        // hazard, so the level this dispatch was configured with is restored
        // by hand. Nothing is listening — `log`'s default logger is a no-op —
        // but the macros evaluate, and the one thing that *is* guaranteed to
        // reach the user, the stderr line below, says what happened.
        eprintln!(
            "Failed to initialise logger: {}. Log output is disabled for this run; \
             log macros still evaluate their arguments.",
            e
        );
        log::set_max_level(LOG_LEVEL);
    }

    // Install a panic hook that writes to the log file so crashes are captured.
    let crash_log_dir = log_dir();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!(
            "[{} PANIC] {}\nBacktrace:\n{:?}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            info,
            std::backtrace::Backtrace::force_capture(),
        );
        eprintln!("{}", msg);
        if let Some(ref dir) = crash_log_dir {
            let crash_path = dir.join("triple-c.log");
            let _ = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&crash_path)
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "{}", msg)
                });
        }
    }));

    if let Some((ref path, _)) = log_file_path {
        log::info!("Logging to {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logger_that_could_not_be_installed_still_leaves_the_macros_evaluating() {
        // H2: `log::info!(…)` expands to `if Info <= max_level() { … }`, so at
        // `LevelFilter::Off` the arguments are never evaluated. `fern` returns
        // before `set_max_level` when `apply()` fails, which leaves exactly
        // that state — and a call site that folded an effect into an argument
        // list then stops performing it, app-wide, because a log file could not
        // be opened. The failure branch restores the level for that reason.
        //
        // Asserted on the level itself rather than by driving `init`, which
        // installs a process-global logger and a panic hook and can only run
        // once per process.
        assert_ne!(LOG_LEVEL, log::LevelFilter::Off);

        // The property that makes the above worth asserting, demonstrated
        // against the macro itself: a side effect in an argument list runs only
        // while the level admits the record.
        let mut ran = false;
        let effect = |v: &mut bool| {
            *v = true;
            0
        };
        let previous = log::max_level();
        log::set_max_level(log::LevelFilter::Off);
        log::info!("{}", effect(&mut ran));
        assert!(!ran, "the premise is wrong: arguments evaluated at LevelFilter::Off");
        log::set_max_level(LOG_LEVEL);
        log::info!("{}", effect(&mut ran));
        assert!(ran, "arguments did not evaluate at the level this module configures");
        log::set_max_level(previous);
    }
}
