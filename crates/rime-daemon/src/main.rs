//! rime-daemon: a standalone librime engine server.
//!
//! * Owns librime exclusively (RimeInitialize once, RimeFinalize at exit),
//!   so the LevelDB user dictionary is only ever opened by this process.
//! * Listens on a unix socket; **each client connection maps to one librime
//!   session** (e.g. one Neovim instance).
//! * Speaks newline-delimited JSON-RPC over the socket.
//! * Data directories (shared/user/log) are configured once at startup via
//!   environment variables (RIME_SHARED_DATA_DIR / RIME_USER_DATA_DIR /
//!   RIME_LOG_DIR / RIME_SOCKET) and cannot be changed at runtime.
//!   RIME_SHARED_DATA_DIR 未设置时优先使用 `~/.config/rime`（若存在），
//!   否则回退 `~/.local/share/rime`——与 rime-cli 之前的拉起逻辑一致。
//! * Runs an automatic deployment at startup (full build when `build/` has no
//!   `.bin` artifacts, incremental check otherwise) on a background thread;
//!   clients connecting later never trigger deployment themselves.
//! * Runs until SIGTERM/SIGINT — it is a resident daemon, not tied to any
//!   client's lifetime.
//!
//! librime's C API is not thread-safe; every call is serialized through
//! [`RIME_LOCK`] (the maintenance thread uses librime's own deployer thread,
//! which librime is designed to run alongside session calls).

mod protocol;

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rime_sys::{
    build_traits, cstr_to_string, rime_struct_init, RimeApi, RimeCommit, RimeContext,
    RimeSchemaList, RimeSessionId, RimeTraits, TraitsOwned, Bool,
};

use serde_json::{json, Value};

/// All librime calls are serialized through this lock.
static RIME_LOCK: Mutex<()> = Mutex::new(());

/// Set by the SIGTERM/SIGINT handler; the accept loop polls it to exit.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// True while the startup auto-deployment is running (including librime's
/// synchronous preparation phase, during which `is_maintenance_mode` may
/// already report false). `maintenance_mode` RPC merges this flag.
static AUTO_DEPLOYING: AtomicBool = AtomicBool::new(false);

/// Ping timeout when checking for an existing daemon.
const PING_TIMEOUT: Duration = Duration::from_millis(300);

// macOS 标准信号编号（不依赖 libc crate，避免其 #[link(name = "iconv")] 引入 -liconv）。
const SIGTERM: i32 = 15;
const SIGINT: i32 = 2;

type SigHandler = extern "C" fn(i32);

unsafe extern "C" {
    #[link_name = "signal"]
    fn c_signal(signum: i32, handler: SigHandler) -> SigHandler;
}

extern "C" fn handle_signal(_sig: i32) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    unsafe {
        c_signal(SIGTERM, handle_signal);
        c_signal(SIGINT, handle_signal);
    }
}

#[derive(Clone)]
struct Config {
    shared_data_dir: PathBuf,
    user_data_dir: PathBuf,
    log_dir: PathBuf,
    socket: PathBuf,
    min_log_level: i32,
}

fn expand(path: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    if let Some(rest) = path.strip_prefix("~/") {
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(path)
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// RIME_SHARED_DATA_DIR 未设置时的解析：优先 `~/.config/rime`（Rime 的
/// 常见独立安装位置），否则回退到 librime 的默认共享目录。
fn preferred_shared_data_dir() -> PathBuf {
    let config = expand("~/.config/rime");
    if config.is_dir() {
        return config;
    }
    expand("~/.local/share/rime")
}

impl Config {
    fn from_env() -> Config {
        let log_dir = expand(&env_or("RIME_LOG_DIR", "~/.local/state/rime.nvim"));
        let socket = std::env::var("RIME_SOCKET")
            .map(|s| expand(&s))
            .unwrap_or_else(|_| {
            std::env::var("XDG_RUNTIME_DIR")
                .map(|d| PathBuf::from(d).join("rime-daemon.sock"))
                .unwrap_or_else(|_| log_dir.join("rime-daemon.sock"))
        });
        let shared_data_dir = match std::env::var("RIME_SHARED_DATA_DIR") {
            Ok(p) => expand(&p),
            Err(_) => preferred_shared_data_dir(),
        };
        Config {
            shared_data_dir,
            user_data_dir: expand(&env_or("RIME_USER_DATA_DIR", "~/.local/share/rime.nvim")),
            log_dir,
            socket,
            min_log_level: env_or("RIME_MIN_LOG_LEVEL", "3").parse().unwrap_or(3),
        }
    }
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "?".into())
}

fn log(config: &Config, msg: &str) {
    let line = format!("[{}] {}", timestamp(), msg);
    eprintln!("{line}");
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.log_dir.join("rime-daemon.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Check whether another daemon already answers on `path`.
fn daemon_alive(path: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(path) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(PING_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PING_TIMEOUT));
    let mut stream = stream;
    if stream.write_all(b"{\"id\":0,\"method\":\"ping\",\"params\":{}}\n").is_err() {
        return false;
    }
    let mut line = String::new();
    let got_pong = BufReader::new(stream)
        .read_line(&mut line)
        .map(|_| line.contains("pong"))
        .unwrap_or(false);
    got_pong
}

fn bind_socket(path: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if daemon_alive(path) {
                eprintln!(
                    "rime-daemon: another instance is already running on {}",
                    path.display()
                );
                process::exit(0);
            }
            // Stale socket from a crashed daemon: remove and retry.
            log(
                &Config::from_env(),
                &format!("removing stale socket {}", path.display()),
            );
            let _ = fs::remove_file(path);
            UnixListener::bind(path).map_err(|e| format!("bind {}: {e}", path.display()))
        }
        Err(e) => Err(format!("bind {}: {e}", path.display())),
    }
}

/// Verify every api function we depend on is present in this librime build.
fn check_api(api: &'static RimeApi) {
    let required = [
        api.initialize.is_some(),
        api.finalize.is_some(),
        api.start_maintenance.is_some(),
        api.is_maintenance_mode.is_some(),
        api.join_maintenance_thread.is_some(),
        api.create_session.is_some(),
        api.destroy_session.is_some(),
        api.process_key.is_some(),
        api.commit_composition.is_some(),
        api.clear_composition.is_some(),
        api.get_commit.is_some(),
        api.free_commit.is_some(),
        api.get_context.is_some(),
        api.free_context.is_some(),
        api.get_schema_list.is_some(),
        api.free_schema_list.is_some(),
        api.get_current_schema.is_some(),
        api.select_schema.is_some(),
    ];
    if required.iter().any(|ok| !ok) {
        eprintln!("rime-daemon: librime build is missing required API functions");
        process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// RPC dispatch
// ---------------------------------------------------------------------------

fn schema_list(api: &'static RimeApi) -> Result<Value, String> {
    let mut list: RimeSchemaList = unsafe { std::mem::zeroed() };
    let ptr = &mut list as *mut RimeSchemaList;
    if unsafe { api.get_schema_list.expect("get_schema_list")(ptr) } == 0 {
        return Err("cannot get schema list".into());
    }
    let out = unsafe {
        let size = (*ptr).size;
        let mut arr = Vec::with_capacity(size);
        for i in 0..size {
            let item = &*(*ptr).list.add(i);
            arr.push(json!({
                "schema_id": cstr_to_string(item.schema_id),
                "name": cstr_to_string(item.name),
            }));
        }
        api.free_schema_list.expect("free_schema_list")(ptr);
        Value::Array(arr)
    };
    Ok(out)
}

fn current_schema(api: &'static RimeApi, session: RimeSessionId) -> Result<Value, String> {
    // c_char: macOS/Linux 上是 i8，Android 上是 u8（Rust 1.64 起）
    let mut buf = [0 as std::ffi::c_char; 1024];
    if unsafe {
        api.get_current_schema.expect("get_current_schema")(
            session,
            buf.as_mut_ptr(),
            buf.len(),
        )
    } == 0
    {
        return Err("no current schema".into());
    }
    Ok(json!(unsafe { cstr_to_string(buf.as_ptr()) }))
}

fn get_commit(api: &'static RimeApi, session: RimeSessionId) -> Result<Value, String> {
    let mut commit: RimeCommit = unsafe { std::mem::zeroed() };
    let ptr = &mut commit as *mut RimeCommit;
    unsafe { rime_struct_init(ptr) };
    if unsafe { api.get_commit.expect("get_commit")(session, ptr) } == 0 {
        return Err("no commit text".into());
    }
    let text = unsafe { cstr_to_string((*ptr).text) };
    unsafe { api.free_commit.expect("free_commit")(ptr) };
    Ok(json!({ "text": text }))
}

fn get_context(api: &'static RimeApi, session: RimeSessionId) -> Result<Value, String> {
    let mut ctx: RimeContext = unsafe { std::mem::zeroed() };
    let ptr = &mut ctx as *mut RimeContext;
    unsafe { rime_struct_init(ptr) };
    if unsafe { api.get_context.expect("get_context")(session, ptr) } == 0 {
        return Err("no context".into());
    }
    let out = unsafe {
        let comp = &(*ptr).composition;
        let menu = &(*ptr).menu;
        let mut candidates = Vec::with_capacity(menu.num_candidates.max(0) as usize);
        for i in 0..menu.num_candidates.max(0) as usize {
            let cand = &*menu.candidates.add(i);
            candidates.push(json!({
                "text": cstr_to_string(cand.text),
                "comment": cstr_to_string(cand.comment),
            }));
        }
        json!({
            "composition": {
                "length": comp.length,
                "cursor_pos": comp.cursor_pos,
                "sel_start": comp.sel_start,
                "sel_end": comp.sel_end,
                "preedit": cstr_to_string(comp.preedit),
            },
            "menu": {
                "page_size": menu.page_size,
                "page_no": menu.page_no,
                "is_last_page": menu.is_last_page != 0,
                "highlighted_candidate_index": menu.highlighted_candidate_index,
                "num_candidates": menu.num_candidates,
                "select_keys": cstr_to_string(menu.select_keys),
                "candidates": candidates,
            },
        })
    };
    unsafe { api.free_context.expect("free_context")(ptr) };
    Ok(out)
}

fn dispatch(line: &str, session: &mut RimeSessionId, api: &'static RimeApi) -> String {
    let (id, method, params) = match protocol::parse_request(line) {
        Some(p) => p,
        None => return protocol::error(None, -32700, "parse error"),
    };

    // Serialize all librime calls.
    let _guard = RIME_LOCK.lock().unwrap();

    // 懒创建 session：部署（maintenance_*）必须在任何引擎启动之前完成，
    // 否则引擎会加载还不存在的 build/ 配置并缓存空结果，导致后续部署失败。
    // 插件流程是 先 deploy 再输入，因此第一个 session_* 请求才建 session。
    if method.starts_with("session_") && *session == 0 {
        *session = unsafe { api.create_session.expect("create_session")() };
    }

    let result = match method.as_str() {
        "ping" => Ok(json!("pong")),
        "traits" => Ok(Value::Null),
        "maintenance_start" => {
            let full = params.get("full").and_then(Value::as_bool).unwrap_or(false);
            let ok = unsafe { api.start_maintenance.expect("start_maintenance")(full as Bool) };
            Ok(json!(ok != 0))
        }
        "maintenance_join" => {
            unsafe { api.join_maintenance_thread.expect("join_maintenance_thread")() };
            Ok(json!(true))
        }
        "maintenance_mode" => {
            let on = unsafe { api.is_maintenance_mode.expect("is_maintenance_mode")() };
            // 自动部署的同步准备阶段 is_maintenance_mode 可能仍为 false，
            // 合并 AUTO_DEPLOYING 让客户端轮询能准确等到部署真正完成。
            Ok(json!(on != 0 || AUTO_DEPLOYING.load(Ordering::SeqCst)))
        }
        "schema_list" => schema_list(api).map(|v| json!(v)),
        "session_current_schema" => current_schema(api, *session).map(|v| json!(v)),
        "session_select_schema" => {
            let schema_id = params.get("schema_id").and_then(Value::as_str).unwrap_or("");
            let ok = unsafe {
                api.select_schema.expect("select_schema")(*session, schema_id.as_ptr() as *const _)
            };
            Ok(json!(ok != 0))
        }
        "session_process_key" => {
            let code = params.get("code").and_then(Value::as_i64).unwrap_or(0) as i32;
            let mask = params.get("mask").and_then(Value::as_i64).unwrap_or(0) as i32;
            let handled = unsafe { api.process_key.expect("process_key")(*session, code, mask) };
            // True = key not consumed by rime, pass through to the editor.
            Ok(json!(handled != 0))
        }
        "session_commit_composition" => {
            let ok = unsafe { api.commit_composition.expect("commit_composition")(*session) };
            Ok(json!(ok != 0))
        }
        "session_clear_composition" => {
            unsafe { api.clear_composition.expect("clear_composition")(*session) };
            Ok(Value::Null)
        }
        "session_get_commit" => get_commit(api, *session).map(|v| json!(v)),
        "session_get_context" => get_context(api, *session).map(|v| json!(v)),
        other => Err(format!("unknown method: {other}")),
    };

    match result {
        Ok(value) => protocol::response(id, value),
        Err(message) => protocol::error(id, -32601, &message),
    }
}

fn handle_connection(stream: UnixStream, api: &'static RimeApi) {
    // macOS/BSD 上 accept() 出的 socket 会继承 listener 的非阻塞标志，
    // 必须显式恢复阻塞模式，否则 read_line 会立刻返回 EAGAIN。
    let _ = stream.set_nonblocking(false);

    // session 懒创建，首个 session_* RPC 时由 dispatch 建立。
    let mut session: RimeSessionId = 0;

    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut writer = stream;

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,      // EOF: client disconnected
            Err(_) => break,     // read error
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let reply = dispatch(&line, &mut session, api);
        if writer.write_all(reply.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }

    if session != 0 {
        let _guard = RIME_LOCK.lock().unwrap();
        unsafe { api.destroy_session.expect("destroy_session")(session) };
    }
}

/// 启动时自动部署：build/ 无 .bin 产物则全量，否则增量检测。
/// 在后台线程运行，不阻塞 socket 服务；客户端连接时部署通常已完成。
fn auto_deploy(api: &'static RimeApi, config: &Config) {
    let config = config.clone();
    let build_dir = config.user_data_dir.join("build");
    let has_bins = std::fs::read_dir(&build_dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                e.path().extension().and_then(|s| s.to_str()) == Some("bin")
            })
        })
        .unwrap_or(false);
    let full = !has_bins;
    log(&config, &format!("auto deploy (full={full})"));
    AUTO_DEPLOYING.store(true, Ordering::SeqCst);
    thread::spawn(move || {
        // 不在 RIME_LOCK 内：librime 的 deployer 自带后台线程，
        // 设计上允许维护与 session 调用并行（维护期间引擎 disabled）。
        unsafe {
            api.start_maintenance.expect("start_maintenance")(full as Bool);
            api.join_maintenance_thread.expect("join_maintenance_thread")();
        }
        AUTO_DEPLOYING.store(false, Ordering::SeqCst);
        log(&config, "auto deploy finished");
    });
}

fn main() {
    install_signal_handlers();
    let config = Config::from_env();

    // Directories must exist before RimeInitialize: glog needs a log dir,
    // deploy writes build/ into user_data_dir.
    for dir in [&config.shared_data_dir, &config.user_data_dir, &config.log_dir] {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("rime-daemon: cannot create {}: {e}", dir.display());
            process::exit(1);
        }
    }

    let listener = match bind_socket(&config.socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rime-daemon: {e}");
            process::exit(1);
        }
    };

    let api = unsafe { RimeApi::get() };
    check_api(api);

    // Initialize librime with the same traits the old rimeshim used.
    let traits_owned = TraitsOwned {
        shared_data_dir: std::ffi::CString::new(config.shared_data_dir.to_string_lossy().as_ref())
            .expect("shared_data_dir contains NUL"),
        user_data_dir: std::ffi::CString::new(config.user_data_dir.to_string_lossy().as_ref())
            .expect("user_data_dir contains NUL"),
        log_dir: std::ffi::CString::new(config.log_dir.to_string_lossy().as_ref())
            .expect("log_dir contains NUL"),
        distribution_name: std::ffi::CString::new("Rime").unwrap(),
        distribution_code_name: std::ffi::CString::new("rime-cli").unwrap(),
        distribution_version: std::ffi::CString::new("0.0.1").unwrap(),
        app_name: std::ffi::CString::new("rime.rime-cli").unwrap(),
    };
    let traits: RimeTraits = build_traits(&traits_owned, config.min_log_level);
    unsafe { api.initialize.expect("initialize")(&traits as *const RimeTraits as *mut RimeTraits) };
    // Keep the traits alive for the process lifetime.
    let _keep_alive = traits_owned;

    log(&config, &format!("ready, listening on {}", config.socket.display()));

    // 后台线程自动部署（首次全量，之后增量检测）
    auto_deploy(api, &config);

    // 常驻：除非收到 SIGTERM/SIGINT，否则一直服务。
    let _ = listener.set_nonblocking(true);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                thread::spawn(move || handle_connection(stream, api));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if SHUTDOWN.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    log(&config, "received shutdown signal, exiting");
    // finalize 内部会 join 维护线程；若自动部署仍在进行（大词库全量），
    // 会等其完成后才退出。
    let _ = fs::remove_file(&config.socket);
    unsafe { api.finalize.expect("finalize")() };
}
