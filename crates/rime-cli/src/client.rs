//! Newline-delimited JSON-RPC 2.0 client for `rime-daemon` over a unix socket.
//!
//! Mirrors the protocol spoken by `lua/rimeshim.lua` (same methods, same
//! framing). The socket path resolves in the same order as the daemon:
//! `RIME_SOCKET` → `$XDG_RUNTIME_DIR/rime-daemon.sock` →
//! `~/.local/state/rime.nvim/rime-daemon.sock`.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(p)
    }
}

/// Resolve the daemon socket path (same precedence as the daemon itself).
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("RIME_SOCKET") {
        return expand(&p);
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("rime-daemon.sock");
    }
    expand("~/.local/state/rime.nvim/rime-daemon.sock")
}

impl Client {
    fn connect(sock: &Path) -> Result<Client, String> {
        let stream =
            UnixStream::connect(sock).map_err(|e| format!("connect {}: {e}", sock.display()))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        Ok(Client {
            reader,
            writer: stream,
            next_id: 1,
        })
    }

    /// Send one JSON-RPC request and wait for its response.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut buf = payload.to_string().into_bytes();
        buf.push(b'\n');
        self.writer.write_all(&buf).map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;

        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("daemon closed the connection".into());
            }
            let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("rpc error")
                        .to_string());
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // Not our response (should not happen with a single outstanding
            // request, but tolerate out-of-band lines).
        }
    }

    /// librime returns 1 when the key was consumed; 0 = pass through to the
    /// editor (in our case: append to the committed line / handle locally).
    pub fn process_key(&mut self, code: i32, mask: i32) -> Result<bool, String> {
        let v = self.request("session_process_key", json!({ "code": code, "mask": mask }))?;
        Ok(v.as_bool().unwrap_or(false))
    }

    /// Fetch pending commit text; `""` when there is none ("no commit text"
    /// is a normal empty state, reported as an RPC error by the daemon).
    pub fn get_commit(&mut self) -> Result<String, String> {
        match self.request("session_get_commit", json!({})) {
            Ok(v) => Ok(v.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
            Err(_) => Ok(String::new()),
        }
    }

    pub fn get_context(&mut self) -> Result<Value, String> {
        self.request("session_get_context", json!({}))
    }

    pub fn commit_composition(&mut self) -> Result<bool, String> {
        let v = self.request("session_commit_composition", json!({}))?;
        Ok(v.as_bool().unwrap_or(false))
    }

    /// True while the daemon is busy with startup maintenance (auto deploy +
    /// user data sync).
    pub fn maintenance_mode(&mut self) -> Result<bool, String> {
        let v = self.request("maintenance_mode", json!({}))?;
        Ok(v.as_bool().unwrap_or(true))
    }

    pub fn schema_list(&mut self) -> Result<Vec<(String, String)>, String> {
        let v = self.request("schema_list", json!({}))?;
        Ok(v.as_array()
            .map(|arr| {
                arr.iter()
                    .map(|s| {
                        (
                            s.get("schema_id").and_then(Value::as_str).unwrap_or("").to_string(),
                            s.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Auto-connect: connect to a resident daemon; if none is running, spawn one
// (detached, inheriting our environment; config is resolved by the daemon
// itself) and retry until the socket appears.
// ---------------------------------------------------------------------------

fn find_daemon_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RIME_DAEMON_BIN") {
        let p = expand(&p);
        if p.is_file() {
            return Some(p);
        }
    }
    // Same directory as this binary (target/release/ or nix $out/bin/).
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent().map(|d| d.join("rime-daemon"));
        if let Some(s) = sibling {
            if s.is_file() {
                return Some(s);
            }
        }
    }
    // PATH fallback.
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join("rime-daemon");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn spawn_daemon() -> Result<(), String> {
    let bin = find_daemon_bin().ok_or_else(|| {
        "未找到 rime-daemon 可执行文件（可用 RIME_DAEMON_BIN 指定）。\
         \n请先启动 Neovim + rime.nvim，或手动运行 rime-daemon。"
            .to_string()
    })?;
    eprintln!("rime-cli: rime-daemon 未运行，启动 {}", bin.display());
    let mut cmd = Command::new(&bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // 不设置任何 RIME_* 环境变量：配置全部由 rime-daemon 自身解析
    // （继承调用者的环境变量，如 RIME_SOCKET / RIME_SHARED_DATA_DIR 等），
    // 与直接启动 rime-daemon 时的解析逻辑完全一致。
    // Detach into a new session so the daemon outlives this client.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            crate::sys::setsid();
            Ok(())
        });
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("spawn {}: {e}", bin.display()))
}

/// Connect to the daemon, spawning it first if necessary. Retries for a few
/// seconds: the daemon binds its socket before auto-deploy runs, so a fresh
/// launch is reachable almost immediately.
pub fn connect_or_spawn(sock: &Path) -> Result<Client, String> {
    if let Ok(c) = Client::connect(sock) {
        return Ok(c);
    }
    spawn_daemon()?;

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_err = String::new();
    while Instant::now() < deadline {
        match Client::connect(sock) {
            Ok(c) => return Ok(c),
            Err(e) => {
                last_err = e;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(format!(
        "无法连接 rime-daemon（{}）。{last_err}",
        sock.display()
    ))
}
