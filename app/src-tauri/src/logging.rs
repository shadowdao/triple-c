use std::fs;
use std::path::PathBuf;

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
        .level(log::LevelFilter::Info)
        .chain(std::io::stderr());

    if let Some((_path, file)) = &log_file_path {
        dispatch = dispatch.chain(fern::Dispatch::new().chain(file.try_clone().unwrap()));
    }

    if let Err(e) = dispatch.apply() {
        eprintln!("Failed to initialise logger: {}", e);
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
