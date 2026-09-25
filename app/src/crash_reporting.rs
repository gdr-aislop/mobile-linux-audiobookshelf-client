//! Local-only (never phones home) crash observability: a persistent, rotating log file under
//! `AppPaths::logs_dir()`, a panic hook that always captures a full backtrace (no
//! `RUST_BACKTRACE` needed), and local Breakpad-format `.dmp` files for native (signal-level)
//! crashes — a segfault/abort inside the GTK/GStreamer/glib C code this binary links against,
//! which a Rust panic hook can never catch. All of this exists because a deployed build normally
//! has no terminal attached (a phone running Phosh) — without it, a crash today leaves no trace a
//! user could ever hand back in a bug report.
//!
//! Ordinary logging deliberately avoids touching flash on every event: writes accumulate in an
//! in-memory buffer and only reach disk on a coarse periodic timer, on a panic, or on clean
//! shutdown — see [`BufferedLogWriter`].
//!
//! Native crash dumps use an out-of-process design (`crash-handler` + `minidumper`, Embark
//! Studios; local IPC + local file only, never networked): this binary re-execs itself as a tiny
//! crash-server subprocess (see [`crash_server_socket_name`]/[`run_crash_server`]) that receives
//! a crash report over a Unix domain socket and writes the `.dmp` file — safer than writing one
//! from inside the crashing process's own signal handler, where very few operations are actually
//! safe to perform.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// A `Write` implementation that buffers in memory (via `BufWriter`) and only touches the
/// underlying rotating log file when that buffer fills or [`flush`](Self::flush) is called
/// explicitly — see the module doc comment for why. `Clone` is cheap (an `Arc` bump); every clone
/// shares the same underlying buffer/file.
#[derive(Clone)]
struct BufferedLogWriter {
    inner: Arc<Mutex<std::io::BufWriter<RollingFileAppender>>>,
}

/// Ordinary logging traffic over a minute of normal use shouldn't approach this before the
/// periodic timer flushes it anyway; sized generously so a burst of activity still doesn't
/// trigger a mid-buffer disk write.
const LOG_BUFFER_CAPACITY: usize = 64 * 1024;

/// How often the background timer flushes buffered log lines to disk during ordinary operation.
/// Trade-off, stated plainly: a hard kill (`SIGKILL`, a native crash) can lose up to this much of
/// the most recent log output — acceptable because a native crash's authoritative artifact is the
/// `.dmp` file crash dumps write (not this log), and a panic flushes immediately regardless of
/// this timer (see [`install_panic_hook`]).
const PERIODIC_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

impl BufferedLogWriter {
    fn new(appender: RollingFileAppender) -> Self {
        Self { inner: Arc::new(Mutex::new(std::io::BufWriter::with_capacity(LOG_BUFFER_CAPACITY, appender))) }
    }

    fn flush(&self) {
        if let Ok(mut writer) = self.inner.lock() {
            let _ = writer.flush();
        }
    }
}

impl Write for BufferedLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.lock().expect("log writer mutex").write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.lock().expect("log writer mutex").flush()
    }
}

/// Held for the lifetime of `main()`. Dropping this has no special behavior (the buffered writer
/// is happy to just stop being flushed); callers explicitly call [`flush`](Self::flush) at the
/// moments that matter (panic, clean shutdown) rather than relying on `Drop` timing.
#[derive(Clone)]
pub struct LogHandle {
    writer: BufferedLogWriter,
}

impl LogHandle {
    pub fn flush(&self) {
        self.writer.flush();
    }
}

/// Installs the global `tracing` subscriber: a rotating file layer under `paths.logs_dir()`
/// (human-readable, no ANSI color codes — a user attaches this file to a bug report, nobody
/// pipes it through a colorizer) plus an unconditional stdout layer (so `cargo run`'s dev
/// experience is unchanged), both filtered to `info` by default (`RUST_LOG` overrides). Starts a
/// detached background thread that flushes the file layer's buffer every
/// [`PERIODIC_FLUSH_INTERVAL`] — see the module doc comment.
///
/// Must be called before anything else logs (including [`install_panic_hook`]'s hook firing) and
/// after `paths.logs_dir()` exists on disk (`AppPaths::ensure_early_dirs`).
pub fn init_logging(paths: &abs_storage::AppPaths) -> LogHandle {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("abs-app")
        .max_log_files(7)
        .build(paths.logs_dir())
        .expect("build the rotating log file appender");

    let writer = BufferedLogWriter::new(appender);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let file_layer = tracing_subscriber::fmt::layer().with_ansi(false).with_writer({
        let writer = writer.clone();
        move || writer.clone()
    });
    let stdout_layer = tracing_subscriber::fmt::layer();

    tracing_subscriber::registry().with(filter).with(file_layer).with(stdout_layer).init();

    {
        let writer = writer.clone();
        std::thread::Builder::new()
            .name("log-flush".to_string())
            .spawn(move || loop {
                std::thread::sleep(PERIODIC_FLUSH_INTERVAL);
                writer.flush();
            })
            .expect("spawn the periodic log-flush thread");
    }

    LogHandle { writer }
}

/// Replaces the default panic hook with one that always captures a full backtrace
/// (`std::backtrace::Backtrace::force_capture` ignores `RUST_BACKTRACE` entirely, unlike the
/// default hook) and logs it via `tracing::error!` — reaching the log file from [`init_logging`]
/// even with no terminal attached — then flushes immediately (a panic is exactly the moment this
/// data matters most; it must not wait for the periodic timer). The previous hook still runs
/// afterwards, so default terminal output on an attached-terminal desktop run is unchanged, as is
/// normal unwind/abort behavior (this hook only adds a logging side effect).
pub fn install_panic_hook(log_handle: LogHandle) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(%backtrace, "panic: {info}");
        log_handle.flush();
        previous(info);
    }));
}

const CRASH_SERVER_FLAG_PREFIX: &str = "--crash-handler-server=";

/// If the process was invoked with the hidden crash-server flag, returns the socket name it
/// should serve on. `main()` checks this before anything else (even `--version`) — a real user
/// never passes this flag; it exists only so this binary can re-exec itself as the crash-dump
/// server (see the module doc comment).
pub fn crash_server_socket_name() -> Option<String> {
    std::env::args().find_map(|arg| arg.strip_prefix(CRASH_SERVER_FLAG_PREFIX).map(str::to_string))
}

/// Runs this process as the crash-dump server and never returns (calls `std::process::exit` once
/// the server's message loop ends). Must be called before any other startup side effect — no
/// database, no GStreamer, no GTK application — since this is purely an internal IPC responder,
/// never a real app instance a user interacts with.
pub fn run_crash_server(socket_name: &str, crash_dumps_dir: PathBuf) -> ! {
    struct Handler {
        crash_dumps_dir: PathBuf,
    }

    impl minidumper::ServerHandler for Handler {
        fn create_minidump_file(&self) -> Result<(std::fs::File, PathBuf), std::io::Error> {
            let _ = std::fs::create_dir_all(&self.crash_dumps_dir);
            let pid = std::process::id();
            let unix_secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
            let path = self.crash_dumps_dir.join(format!("crash-{unix_secs}-{pid}.dmp"));
            let file = std::fs::File::create(&path)?;
            Ok((file, path))
        }

        fn on_minidump_created(&self, result: Result<minidumper::MinidumpBinary, minidumper::Error>) -> minidumper::LoopAction {
            match result {
                Ok(mut minidump) => {
                    let _ = minidump.file.flush();
                    tracing::info!(path = ?minidump.path, "wrote a native-crash minidump");
                }
                Err(err) => tracing::warn!(%err, "failed to write a native-crash minidump"),
            }
            minidumper::LoopAction::Exit
        }

        fn on_message(&self, _kind: u32, _buffer: Vec<u8>) {}

        /// The main app process closes its end of the socket as soon as it exits, whether that's
        /// a normal quit or a hard `SIGKILL` — the OS closes file descriptors on process
        /// termination regardless of cause. Exiting here once the last client is gone is what
        /// prevents this server from outliving the app it's monitoring, with no separate
        /// "clean shutdown" message needed.
        fn on_client_disconnected(&self, num_clients: usize) -> minidumper::LoopAction {
            if num_clients == 0 { minidumper::LoopAction::Exit } else { minidumper::LoopAction::Continue }
        }
    }

    let socket = minidumper::SocketName::abstract_namespace(socket_name);
    let mut server = minidumper::Server::with_name(socket).expect("create the crash-dump IPC server");
    let shutdown = std::sync::atomic::AtomicBool::new(false);
    if let Err(err) = server.run(Box::new(Handler { crash_dumps_dir }), &shutdown, None) {
        tracing::warn!(%err, "crash-dump server exited with an error");
    }
    std::process::exit(0);
}

/// Held for the lifetime of `main()`'s body — dropping this detaches the signal handler and lets
/// the crash-server subprocess's socket see a disconnect (which makes it exit; see
/// `run_crash_server`'s `on_client_disconnected`).
pub struct ClientHandles {
    _handler: crash_handler::CrashHandler,
    _client: Arc<minidumper::Client>,
    /// Not otherwise touched — the server subprocess's exit is driven entirely by
    /// `run_crash_server`'s `on_client_disconnected`, triggered by `_client`'s socket closing
    /// whenever this process exits (cleanly or otherwise). Kept only so this handle, not a bare
    /// PID, is what represents "the server we spawned."
    _server_process: std::process::Child,
}

/// Spawns this same binary as a crash-dump server subprocess (which resolves its own `AppPaths`
/// for `crash_dumps_dir()` — nothing needs to be passed to it) and attaches a signal handler that
/// forwards native (SIGSEGV/SIGABRT/SIGBUS/SIGILL/SIGFPE) crashes to it. Any failure along the
/// way (subprocess spawn refused, socket connect timeout, signal handler attach refused — e.g.
/// under a sandboxed/seccomp environment) degrades to a warning and `None`, exactly like
/// `abs_player::init()`'s existing GStreamer-failure handling in `main.rs` — crash-dump capture
/// is never allowed to block the app from starting.
pub fn attach_crash_handler() -> Option<ClientHandles> {
    let socket_name = format!("abs-app-crash-{}", std::process::id());

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(%err, "crash-dump capture unavailable: couldn't resolve the running executable's path");
            return None;
        }
    };

    let mut server_process = match std::process::Command::new(&exe).arg(format!("{CRASH_SERVER_FLAG_PREFIX}{socket_name}")).spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(%err, "crash-dump capture unavailable: couldn't spawn the crash-dump server");
            return None;
        }
    };

    let socket = minidumper::SocketName::abstract_namespace(&socket_name);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    let client = loop {
        match minidumper::Client::with_name(socket) {
            Ok(client) => break Some(client),
            Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => {
                tracing::warn!(%err, "crash-dump capture unavailable: couldn't connect to the crash-dump server");
                break None;
            }
        }
    };
    let Some(client) = client else {
        let _ = server_process.kill();
        return None;
    };
    let client = Arc::new(client);

    let handler = {
        let client_for_crash = client.clone();
        #[allow(unsafe_code)]
        unsafe {
            crash_handler::CrashHandler::attach(crash_handler::make_crash_event(move |crash_context: &crash_handler::CrashContext| {
                let _ = client_for_crash.ping();
                crash_handler::CrashEventResult::Handled(client_for_crash.request_dump(crash_context).is_ok())
            }))
        }
    };
    let handler = match handler {
        Ok(handler) => handler,
        Err(err) => {
            tracing::warn!(%err, "crash-dump capture unavailable: couldn't attach the signal handler");
            let _ = server_process.kill();
            return None;
        }
    };

    // Restricts who may inspect this process for crash information to the server we just spawned
    // (Linux-only concept — matches the upstream `minidumper` example).
    handler.set_ptracer(Some(server_process.id()));

    Some(ClientHandles { _handler: handler, _client: client, _server_process: server_process })
}
