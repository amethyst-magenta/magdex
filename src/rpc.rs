use std::{io::Write as _, path::PathBuf, process::Stdio};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};

#[derive(Debug)]
pub enum Incoming {
    Message(Value),
    Disconnected(String),
}

pub struct RpcClient {
    writer: mpsc::UnboundedSender<String>,
    pub incoming: mpsc::UnboundedReceiver<Incoming>,
    child: Child,
    next_id: u64,
    debug: bool,
}

impl RpcClient {
    pub async fn spawn(debug: bool, default_mode_request_user_input: bool) -> Result<Self> {
        let mut command = Command::new("codex");
        command.arg("app-server");
        if default_mode_request_user_input {
            command.args(["--enable", "default_mode_request_user_input"]);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().context(
            "cannot start `codex app-server`; install the official Codex CLI and ensure `codex` is in PATH",
        )?;
        let stdin = child.stdin.take().context("app-server stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("app-server stdout unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("app-server stderr unavailable")?;

        let (writer, mut write_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = write_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    break;
                }
            }
            let _ = stdin.shutdown().await;
        });

        let (output_tx, incoming) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if debug {
                            debug_log("<", &line);
                        }
                        if let Ok(value) = serde_json::from_str::<Value>(&line) {
                            if output_tx.send(Incoming::Message(value)).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = output_tx.send(Incoming::Disconnected(
                            "Codex backend closed its output".into(),
                        ));
                        break;
                    }
                    Err(error) => {
                        let _ = output_tx.send(Incoming::Disconnected(error.to_string()));
                        break;
                    }
                }
            }
        });

        let log_path = log_path("app-server.log");
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut log = if let Some(parent) = log_path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .await
                    .ok()
            } else {
                None
            };
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(file) = log.as_mut() {
                    let _ = file.write_all(line.as_bytes()).await;
                    let _ = file.write_all(b"\n").await;
                }
            }
        });

        if debug {
            debug_log("#", "started codex app-server");
        }

        Ok(Self {
            writer,
            incoming,
            child,
            next_id: 1,
            debug,
        })
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"method": method, "params": params}))
    }

    pub fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.send(json!({"id": id, "result": result}))
    }

    pub fn respond_error(&self, id: Value, message: &str) -> Result<()> {
        self.send(json!({
            "id": id,
            "error": {"code": -32601, "message": message}
        }))
    }

    fn send(&self, value: Value) -> Result<()> {
        if self.debug {
            debug_log(">", &value.to_string());
        }
        self.writer
            .send(value.to_string())
            .context("Codex backend writer stopped")
    }

    pub async fn shutdown(mut self) {
        drop(self.writer);
        if tokio::time::timeout(std::time::Duration::from_millis(700), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

fn log_path(name: &str) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("magdex")
        .join(name)
}

fn debug_log(direction: &str, message: &str) {
    let path = log_path("magdex.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let _ = writeln!(file, "{timestamp} {direction} {message}");
    }
}
