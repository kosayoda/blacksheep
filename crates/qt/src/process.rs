//! `Process` element: run a command, stream its output line by line.
//!
//! ```qml
//! Process {
//!     command: ["sh", "-c", "date +%H:%M"]
//!     running: true
//!     onStdout: line => label.text = line
//! }
//! ```

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[namespace = "blacksheep"]
        #[qproperty(QStringList, command, READ, WRITE = set_command, NOTIFY)]
        #[qproperty(QString, working_directory, cxx_name = "workingDirectory")]
        /// Setting this starts or terminates the process; it turns false on exit.
        #[qproperty(bool, running, READ, WRITE = set_running, NOTIFY)]
        #[qproperty(i32, exit_code, cxx_name = "exitCode", READ, NOTIFY)]
        type Process = super::ProcessRust;

        /// A line written to standard output, without the newline.
        #[qsignal]
        fn stdout(self: Pin<&mut Process>, line: QString);
        /// A line written to standard error, without the newline.
        #[qsignal]
        fn stderr(self: Pin<&mut Process>, line: QString);
        #[qsignal]
        fn exited(self: Pin<&mut Process>, code: i32);

        fn set_command(self: Pin<&mut Process>, command: QStringList);
        fn set_running(self: Pin<&mut Process>, running: bool);

        #[qinvokable]
        fn start(self: Pin<&mut Process>);
        /// Send SIGTERM.
        #[qinvokable]
        fn stop(self: Pin<&mut Process>);
        /// Write to the process's standard input.
        #[qinvokable]
        fn write(self: Pin<&mut Process>, text: &QString);
    }

    impl cxx_qt::Threading for Process {}
}

use std::pin::Pin;
use std::process::Stdio;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QStringList};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::service::set_property;

enum Output {
    Stdout(String),
    Stderr(String),
    Exited(i32),
}

enum Control {
    Write(String),
    Terminate,
}

#[derive(Default)]
pub struct ProcessRust {
    command: QStringList,
    working_directory: QString,
    running: bool,
    exit_code: i32,

    control: Option<mpsc::UnboundedSender<Control>>,
    tasks: Vec<AbortHandle>,
}

impl Drop for ProcessRust {
    fn drop(&mut self) {
        if let Some(control) = &self.control {
            let _ = control.send(Control::Terminate);
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl qobject::Process {
    fn set_command(mut self: Pin<&mut Self>, command: QStringList) {
        set_property!(self, command, command);
        // `running: true` may have been set before `command` in QML.
        if self.running && self.control.is_none() {
            self.start();
        }
    }

    fn set_running(mut self: Pin<&mut Self>, running: bool) {
        if running {
            set_property!(self, running, true);
            if self.control.is_none() {
                self.start();
            }
        } else {
            self.stop();
        }
    }

    fn start(mut self: Pin<&mut Self>) {
        if self.control.is_some() {
            return;
        }
        let argv: Vec<String> = self.command.iter().map(|s| s.to_string()).collect();
        let Some((program, args)) = argv.split_first() else {
            // Nothing to run yet; wait for a command.
            return;
        };
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !self.working_directory.is_empty() {
            command.current_dir(self.working_directory.to_string());
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!("process: cannot start {program}: {err}");
                set_property!(self, running, false);
                return;
            }
        };

        let (out_tx, out_rx) = mpsc::channel::<Output>(256);
        let (control_tx, mut control_rx) = mpsc::unbounded_channel::<Control>();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut stdin = child.stdin.take();
        let pid = child.id();

        let reader = |stream: Option<tokio::process::ChildStdout>, tx: mpsc::Sender<Output>| async move {
            let Some(stream) = stream else { return };
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(Output::Stdout(line)).await.is_err() {
                    break;
                }
            }
        };
        let err_reader = |stream: Option<tokio::process::ChildStderr>, tx: mpsc::Sender<Output>| async move {
            let Some(stream) = stream else { return };
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(Output::Stderr(line)).await.is_err() {
                    break;
                }
            }
        };

        let supervisor = tokio::spawn(async move {
            let out = tokio::join!(
                reader(stdout, out_tx.clone()),
                err_reader(stderr, out_tx.clone()),
                async {
                    loop {
                        tokio::select! {
                            status = child.wait() => {
                                use std::os::unix::process::ExitStatusExt as _;
                                // Same convention as shells: 128 + signal number.
                                return match status {
                                    Ok(status) => status
                                        .code()
                                        .or_else(|| status.signal().map(|sig| 128 + sig))
                                        .unwrap_or(-1),
                                    Err(_) => -1,
                                };
                            }
                            control = control_rx.recv() => match control {
                                Some(Control::Write(text)) => {
                                    if let Some(stdin) = stdin.as_mut()
                                        && let Err(err) = stdin.write_all(text.as_bytes()).await
                                    {
                                        tracing::warn!("process: stdin write failed: {err}");
                                    }
                                }
                                Some(Control::Terminate) | None => {
                                    if let Some(pid) = pid {
                                        // SAFETY: plain syscall on a pid we own.
                                        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                                    }
                                }
                            }
                        }
                    }
                }
            );
            let _ = out_tx.send(Output::Exited(out.2)).await;
        });

        let consumer = crate::service::forward(self.qt_thread(), out_rx, apply);
        let mut rust = self.as_mut().rust_mut();
        rust.control = Some(control_tx);
        rust.tasks = vec![supervisor.abort_handle(), consumer];
        drop(rust);
        set_property!(self, running, true);
    }

    fn stop(self: Pin<&mut Self>) {
        if let Some(control) = &self.control {
            let _ = control.send(Control::Terminate);
        }
    }

    fn write(self: Pin<&mut Self>, text: &QString) {
        if let Some(control) = &self.control {
            let _ = control.send(Control::Write(text.to_string()));
        }
    }
}

fn apply(mut obj: Pin<&mut qobject::Process>, output: Output) {
    match output {
        Output::Stdout(line) => obj.as_mut().stdout(QString::from(line.as_str())),
        Output::Stderr(line) => obj.as_mut().stderr(QString::from(line.as_str())),
        Output::Exited(code) => {
            obj.as_mut().rust_mut().control = None;
            obj.as_mut().rust_mut().tasks.clear();
            set_property!(obj, exit_code, code);
            set_property!(obj, running, false);
            obj.as_mut().exited(code);
        }
    }
}
