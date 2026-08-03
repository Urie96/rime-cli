//! rime-cli: 终端版 Rime 输入法客户端——键盘 → tmux pane 转发器。
//!
//! 启动时自动连接常驻的 rime-daemon（未运行则自动拉起；可执行文件用
//! `RIME_DAEMON_BIN` 指定，其余配置由 rime-daemon 启动时自行解析，
//! 不设置任何环境变量）。
//!
//! 转发目标二选一：
//!   - stdout（默认）：上屏中文与未消费按键的原始字节写入 stdout；
//!   - `--exec <模板>`：每次转发直接执行解析后的命令（不经 sh，不再写 stdout），
//!     模板中 `{}` 被替换为本次转发的字符（作为单独参数）。
//!     例：`rime-cli --exec 'tmux send-keys -t %1 -l {}'`。
//!
//! 界面（2 行，画在 stderr）：
//!   第 1 行：preedit（拼音串，光标为终端竖线光标）
//!   第 2 行：候选词（数字选字、翻页等）
//!
//! 退出：`Ctrl-C`（不转发）；外部 SIGTERM/SIGINT 亦可。

mod client;
mod sys;
mod term;
mod ui;

use std::ffi::OsString;
use std::io::Write;
use std::process::Command;

use client::Client;
use serde_json::json;

/// 转发目标。
enum Sink {
    /// 直接写入 stdout（默认）。
    Stdout,
    /// 每次转发直接执行解析后的命令（不经 sh）。模板支持两个占位符：
    /// `{}` = 字面负载（上屏文本/原始字节）；`{key}` = tmux 键名
    /// （如 `C-d`、`Up`、`Enter`），由 tmux 按目标 pane 的键盘协议编码——
    /// 对启用了 kitty keyboard protocol 的程序（fish 4 / nvim 等）必需。
    Exec(Vec<String>),
}

impl Sink {
    /// `key`：None = 文本事件（上屏文本），Some = 按键事件（透传按键）。
    fn forward(&self, key: Option<&term::Key>, text: &str) {
        match self {
            Sink::Stdout => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
            }
            Sink::Exec(argv) => {
                let mut args: Vec<OsString> = Vec::with_capacity(argv.len());
                for a in argv {
                    let mut out = a.clone();
                    if a.contains("{key}") {
                        // 文本事件：字符键语义（tmux 逐字符发送）；按键事件：键名。
                        // Raw 事件无法表达为键名 → 跳过本次转发。
                        match key {
                            None => out = out.replace("{key}", text),
                            Some(k) => match tmux_key_name(k) {
                                Some(name) => out = out.replace("{key}", &name),
                                None => return,
                            },
                        }
                    }
                    out = out.replace("{}", text);
                    args.push(OsString::from(out));
                }
                if let Some(prog) = args.first() {
                    if let Ok(mut child) = Command::new(prog).args(&args[1..]).spawn() {
                        let _ = child.wait(); // 等待保证转发顺序
                    }
                }
            }
        }
    }
}

/// 按键 → tmux 键名（tmux send-keys 无 `-l` 时按此解析，并按目标 pane 的
/// 键盘协议编码发送，如 `C-d` → kitty 序列 `\x1b[27;4;100~`）。
fn tmux_key_name(key: &term::Key) -> Option<String> {
    match key {
        term::Key::Char(c) => Some(match c {
            ' ' => "Space".into(),
            c => c.to_string(), // 含非 ASCII：tmux 按字符键发送
        }),
        term::Key::Code(code, mask) => {
            let mut s = String::new();
            if mask & term::MOD_CTRL != 0 {
                s.push_str("C-");
            }
            if mask & term::MOD_ALT != 0 {
                s.push_str("M-");
            }
            if mask & term::MOD_SHIFT != 0 {
                s.push_str("S-");
            }
            let base = match *code {
                0x20 => "Space".to_string(),
                term::KEY_RETURN => "Enter".to_string(),
                term::KEY_ESCAPE => "Escape".to_string(),
                term::KEY_BACKSPACE => "Bspace".to_string(),
                term::KEY_TAB => "Tab".to_string(),
                term::KEY_DELETE => "Delete".to_string(),
                term::KEY_INSERT => "Insert".to_string(),
                term::KEY_HOME => "Home".to_string(),
                term::KEY_END => "End".to_string(),
                term::KEY_PAGE_UP => "PageUp".to_string(),
                term::KEY_PAGE_DOWN => "PageDown".to_string(),
                term::KEY_UP => "Up".to_string(),
                term::KEY_DOWN => "Down".to_string(),
                term::KEY_LEFT => "Left".to_string(),
                term::KEY_RIGHT => "Right".to_string(),
                c if (0x20..=0x7e).contains(&c) => {
                    let ch = c as u8 as char;
                    // Ctrl+字母统一小写（tmux 键名不区分大小写）
                    if mask & term::MOD_CTRL != 0 {
                        ch.to_ascii_lowercase().to_string()
                    } else {
                        ch.to_string()
                    }
                }
                _ => return None,
            };
            Some(s + &base)
        }
        term::Key::Raw(_) | term::Key::Quit => None,
    }
}

/// 极简 shell 风格分词：按空白切分，支持 `'...'` 与 `"..."` 引号、反斜杠
/// 转义。不做变量展开/通配符/重定向——命令不经 sh，直接解析执行。
fn print_usage() {
    eprintln!("rime-cli — 键盘 → tmux pane 的 Rime 输入法转发器");
    eprintln!();
    eprintln!("用法: rime-cli [--exec <命令模板>]");
    eprintln!();
    eprintln!("  --exec <模板>   每次转发直接执行解析后的命令（不经 sh，不再写 stdout）；");
    eprintln!("                   模板中 {{}} 替换为字面负载（上屏文本/原始字节），");
    eprintln!("                   {{key}} 替换为 tmux 键名（如 C-d / Up / Enter），");
    eprintln!("                   由 tmux 按目标 pane 的键盘协议编码（fish/nvim 必需）。");
    eprintln!("                   例: rime-cli --exec 'tmux send-keys -t %1 {{key}}'");
    eprintln!("                   例: rime-cli --exec 'tmux send-keys -t %1 -l {{}}'");
    eprintln!("                   也可用环境变量 RIME_EXEC 指定");
    eprintln!("  -h, --help      显示本帮助");
    eprintln!();
    eprintln!("退出: Ctrl-C（不转发）；界面（preedit/候选）画在 stderr。");
}

/// 解析命令行参数，返回 --exec 模板（未指定时回退到 RIME_EXEC）。
/// 模板用 shlex（POSIX 分词）解析成 argv，支持引号/转义。
fn parse_args() -> Option<Vec<String>> {
    let mut args = std::env::args().skip(1);
    let mut exec = std::env::var("RIME_EXEC").ok();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--exec" | "-e" => exec = args.next(),
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                eprintln!("rime-cli: 未知参数: {other}");
                print_usage();
                std::process::exit(2);
            }
        }
    }
    match exec {
        Some(t) => match shlex::split(&t) {
            Some(argv) => {
                if !t.contains("{}") && !t.contains("{key}") {
                    eprintln!("rime-cli: 警告: --exec 模板不含占位符，转发内容将无处安放");
                }
                Some(argv)
            }
            None => {
                eprintln!("rime-cli: --exec 模板引号不匹配: {t}");
                std::process::exit(2);
            }
        },
        None => None,
    }
}

fn main() {
    let exec_argv = parse_args();

    // 1. 启动时自动连接 daemon（未运行则拉起，最多重试约 10s）
    let sock = client::default_socket_path();
    let mut c = match client::connect_or_spawn(&sock) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rime-cli: {e}");
            std::process::exit(1);
        }
    };

    // 2. 等待首次部署完成（新建会话前引擎必须就绪，否则会加载空 build/）
    wait_deployed(&mut c);

    // 3. 进入 raw 终端模式
    term::install_signal_handlers();
    let _raw = match term::enable_raw() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rime-cli: {e}");
            std::process::exit(1);
        }
    };

    // 4. 主循环
    let mut st = ui::State::new();
    st.schema = fetch_schema_name(&mut c);

    let sink = match exec_argv {
        Some(argv) => Sink::Exec(argv),
        None => Sink::Stdout,
    };
    let mut tui = std::io::stderr(); // 界面（preedit/候选）画在 stderr

    loop {
        // 重绘 2 行界面
        let frame = ui::render(&st);
        if tui.write_all(frame.as_bytes()).is_err() || tui.flush().is_err() {
            break;
        }
        let ev = match term::read_key() {
            Ok(Some(e)) => e,
            Ok(None) => break, // EOF
            Err(e) => {
                let _ = tui.write_all(format!("\r\nrime-cli: 读取输入失败: {e}").as_bytes());
                break;
            }
        };
        if ev.key == term::Key::Quit {
            break;
        }
        handle_event(&mut c, &mut st, &sink, ev);
    }

    // 退出：恢复终端（Drop 还原 termios），并恢复光标可见与默认形状
    let _ = tui.write_all(b"\x1b[0m\x1b[?25h\x1b[0 q\r\n");
    let _ = tui.flush();
}

/// 处理一个按键：rime 消费则同步界面并转发上屏文本；否则按原始字节转发。
fn handle_event(c: &mut Client, st: &mut ui::State, sink: &Sink, ev: term::KeyEvent) {
    // 透传按键的负载：原始字节（`{}` 占位符用），lossy 转文本。
    let raw_text = String::from_utf8_lossy(&ev.raw);
    match &ev.key {
        term::Key::Char(ch) if ch.is_ascii() => {
            if c.process_key(*ch as i32, 0).unwrap_or(false) {
                forward_commit(c, st, sink);
            } else {
                sink.forward(Some(&ev.key), &raw_text);
            }
        }
        term::Key::Char(_) => {
            // 非 ASCII（直接输入的中文/符号）：不经 rime，原样转发
            sink.forward(Some(&ev.key), &raw_text);
        }
        term::Key::Code(code, mask) => {
            // ↑/↓ 映射为 PageUp/PageDown 翻候选页（与 rime.nvim 插件一致）；
            // 无候选时 rime 不会消费，仍会按原始字节（方向键）转发。
            let (code, mask) = match (*code, *mask) {
                (term::KEY_UP, 0) => (term::KEY_PAGE_UP, 0),
                (term::KEY_DOWN, 0) => (term::KEY_PAGE_DOWN, 0),
                other => other,
            };
            let handled = c.process_key(code, mask).unwrap_or(false);
            if handled {
                forward_commit(c, st, sink);
            } else if code == term::KEY_RETURN && st.has_preedit() {
                // 有 preedit 且 rime 未消费回车：按输入法惯例本地上屏（转发其上屏文本）
                let _ = c.commit_composition();
                forward_commit(c, st, sink);
            } else {
                sink.forward(Some(&ev.key), &raw_text);
            }
        }
        term::Key::Raw(_) => {
            // 未识别的序列（如 Ctrl+方向键）：不经 rime，原样转发
            sink.forward(Some(&ev.key), &raw_text);
        }
        term::Key::Quit => {}
    }
}

/// 把本次新上屏的文本转发到目标（空文本不写）。文本事件：`{key}` 与 `{}`
/// 都替换为上屏文本（tmux 无 `-l` 时会逐字符当作字符键发送）。
fn forward_commit(c: &mut Client, st: &mut ui::State, sink: &Sink) {
    let text = st.refresh(c);
    if !text.is_empty() {
        sink.forward(None, &text);
    }
}

/// 等待 daemon 完成启动自动部署（仅首次使用较慢）。
fn wait_deployed(c: &mut Client) {
    match c.maintenance_mode() {
        Ok(false) => return,
        Ok(true) => {
            eprintln!("rime-cli: 首次使用，正在部署词库（仅需一次，请稍候）…");
            let start = std::time::Instant::now();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(300));
                match c.maintenance_mode() {
                    Ok(false) => {
                        eprintln!("rime-cli: 部署完成");
                        return;
                    }
                    Ok(true) if start.elapsed().as_secs() > 600 => {
                        eprintln!("rime-cli: 部署超时，请检查 user_data_dir 是否可写");
                        return;
                    }
                    Err(e) => {
                        eprintln!("rime-cli: 查询部署状态失败: {e}");
                        return;
                    }
                    _ => {}
                }
            }
        }
        Err(e) => eprintln!("rime-cli: 查询部署状态失败: {e}"),
    }
}

/// 取当前方案显示名（查 schema_list 拿 name，失败则回退到 schema id）。
fn fetch_schema_name(c: &mut Client) -> Option<String> {
    let id = match c.request("session_current_schema", json!({})) {
        Ok(v) => v.as_str().unwrap_or("").to_string(),
        Err(_) => return None,
    };
    if id.is_empty() {
        return None;
    }
    let name = c
        .schema_list()
        .ok()
        .and_then(|list| list.into_iter().find(|(sid, _)| *sid == id).map(|(_, n)| n));
    Some(name.unwrap_or(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// shlex 分词后，`{}` 占位符应作为一个独立 token，替换时保持为单个参数。
    #[test]
    fn exec_argv_keeps_placeholder_as_token() {
        let argv = shlex::split("tmux send-keys -t %1 -l {}").unwrap();
        assert_eq!(argv, vec!["tmux", "send-keys", "-t", "%1", "-l", "{}"]);
    }

    #[test]
    fn shlex_handles_quotes() {
        assert_eq!(
            shlex::split("a 'b c' d \"e f\" g").unwrap(),
            vec!["a", "b c", "d", "e f", "g"]
        );
        assert!(shlex::split("unclosed '").is_none());
    }

    #[test]
    fn tmux_key_names() {
        use crate::term::{
            Key, MOD_ALT, MOD_CTRL, MOD_SHIFT, KEY_BACKSPACE, KEY_DOWN, KEY_END, KEY_ESCAPE,
            KEY_HOME, KEY_INSERT, KEY_LEFT, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_RETURN, KEY_RIGHT,
            KEY_TAB, KEY_UP,
        };
        assert_eq!(tmux_key_name(&Key::Code(0x52, MOD_CTRL)).unwrap(), "C-r");
        assert_eq!(tmux_key_name(&Key::Code(0x44, MOD_CTRL)).unwrap(), "C-d");
        assert_eq!(tmux_key_name(&Key::Code(0x78, MOD_ALT)).unwrap(), "M-x");
        assert_eq!(tmux_key_name(&Key::Code(0x58, MOD_ALT)).unwrap(), "M-X");
        assert_eq!(tmux_key_name(&Key::Code(0x41, MOD_SHIFT)).unwrap(), "S-A");
        assert_eq!(tmux_key_name(&Key::Code(KEY_UP, 0)).unwrap(), "Up");
        assert_eq!(tmux_key_name(&Key::Code(KEY_UP, MOD_CTRL)).unwrap(), "C-Up");
        assert_eq!(tmux_key_name(&Key::Code(KEY_RETURN, 0)).unwrap(), "Enter");
        assert_eq!(tmux_key_name(&Key::Code(KEY_BACKSPACE, 0)).unwrap(), "Bspace");
        assert_eq!(tmux_key_name(&Key::Code(KEY_ESCAPE, 0)).unwrap(), "Escape");
        assert_eq!(tmux_key_name(&Key::Code(0x20, 0)).unwrap(), "Space");
        assert_eq!(tmux_key_name(&Key::Char(' ')).unwrap(), "Space");
        assert_eq!(tmux_key_name(&Key::Char('a')).unwrap(), "a");
        assert_eq!(tmux_key_name(&Key::Char('我')).unwrap(), "我");
        assert_eq!(tmux_key_name(&Key::Code(KEY_TAB, MOD_SHIFT)).unwrap(), "S-Tab");
        // 方向键/功能键全覆盖
        for (k, n) in [
            (KEY_DOWN, "Down"),
            (KEY_LEFT, "Left"),
            (KEY_RIGHT, "Right"),
            (KEY_PAGE_UP, "PageUp"),
            (KEY_PAGE_DOWN, "PageDown"),
            (KEY_HOME, "Home"),
            (KEY_END, "End"),
            (KEY_INSERT, "Insert"),
        ] {
            assert_eq!(tmux_key_name(&Key::Code(k, 0)).unwrap(), n);
        }
    }
}
