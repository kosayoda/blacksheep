//! Control socket protocol shared by the shell and the `blacksheep msg` CLI.
//!
//! One JSON request per connection, newline terminated, one JSON reply.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Reload the configuration.
    Reload,
    /// Names of all registered `IpcHandler` targets.
    List,
    /// Call `function` on the `IpcHandler` whose target is `target`.
    Call {
        target: String,
        function: String,
        #[serde(default)]
        args: Vec<Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ok", rename_all = "snake_case")]
pub enum Reply {
    #[serde(rename = "true")]
    Ok {
        #[serde(default, skip_serializing_if = "Value::is_null")]
        result: Value,
    },
    #[serde(rename = "false")]
    Err { error: String },
}

/// Socket path, unique per Wayland session.
pub fn socket_path() -> Utf8PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|_| Utf8PathBuf::from("/tmp"));
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "default".into());
    runtime.join("blacksheep").join(format!("{display}.sock"))
}

/// Send one request to a running shell and wait for its reply.
pub async fn send(request: &Request) -> std::io::Result<Reply> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let mut stream = tokio::net::UnixStream::connect(socket_path()).await?;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    stream.shutdown().await?;

    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).await?;
    Ok(serde_json::from_str(&reply)?)
}
