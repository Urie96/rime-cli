//! Raw terminal mode + key event reading (xterm-style escape sequences →
//! X11 keysyms, the same ones the daemon/librime expect; see lua/rime/key.lua).
//!
//! 每个按键事件同时携带终端原始字节：rime 不消费的按键（含组合键、方向键、
//! 未识别的转义序列）会按原字节转发到 stdout，供 tmux pane 等消费方使用。

use crate::sys::{self, consts, PollFd, Termios};
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Set by SIGTERM/SIGINT handlers; the main loop polls it so the terminal is
/// restored by the `RawMode` guard on a clean exit.
pub static TERM_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Saved terminal state so the `RawMode` guard can restore it on exit.
static ORIG_TERMIOS: Mutex<Option<Termios>> = Mutex::new(None);

extern "C" fn handle_signal(_sig: i32) {
    TERM_SHUTDOWN.store(true, Ordering::SeqCst);
}

pub fn install_signal_handlers() {
    unsafe {
        sys::c_signal(sys::SIGTERM, handle_signal);
        sys::c_signal(sys::SIGINT, handle_signal);
    }
}

/// X11 keysyms used by librime (values copied from lua/rime/key.lua).
pub const KEY_BACKSPACE: i32 = 0xff08;
pub const KEY_TAB: i32 = 0xff09;
pub const KEY_RETURN: i32 = 0xff0d;
pub const KEY_ESCAPE: i32 = 0xff1b;
pub const KEY_HOME: i32 = 0xff50;
pub const KEY_LEFT: i32 = 0xff51;
pub const KEY_UP: i32 = 0xff52;
pub const KEY_RIGHT: i32 = 0xff53;
pub const KEY_DOWN: i32 = 0xff54;
pub const KEY_PAGE_UP: i32 = 0xff55;
pub const KEY_PAGE_DOWN: i32 = 0xff56;
pub const KEY_END: i32 = 0xff57;
pub const KEY_INSERT: i32 = 0xff63;
pub const KEY_DELETE: i32 = 0xffff;

/// librime modifier mask (kShiftMask=1, kControlMask=4, kMod1Mask=8).
pub const MOD_SHIFT: i32 = 1;
pub const MOD_CTRL: i32 = 4;
pub const MOD_ALT: i32 = 8;

/// STDIN_FILENO
const STDIN_FILENO: i32 = 0;

/// 解码后的按键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// 可打印字符。ASCII 交给 rime；非 ASCII（如直接输入的中文）原样转发。
    Char(char),
    /// (keysym, modifier mask) 特殊键/组合键，交给 rime 判断是否消费。
    Code(i32, i32),
    /// 无法解码的序列（如 Ctrl+方向键 `\x1b[1;5A`）：不送 rime，原样转发。
    Raw(Vec<u8>),
    /// Ctrl-C：退出（不转发）。
    Quit,
}

/// 一个按键事件：解码结果 + 终端收到的原始字节。
#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub key: Key,
    pub raw: Vec<u8>,
}

/// Raw-mode guard: restores the original termios on drop (clean exit only).
pub struct RawMode;

pub fn enable_raw() -> Result<RawMode, String> {
    let fd = STDIN_FILENO;
    let mut orig: Termios = unsafe { std::mem::zeroed() };
    if unsafe { sys::tcgetattr(fd, &mut orig) } != 0 {
        return Err(sys::last_os_error("tcgetattr"));
    }
    let mut raw = orig;
    raw.c_iflag &= !(consts::IGNBRK
        | consts::BRKINT
        | consts::PARMRK
        | consts::ISTRIP
        | consts::INLCR
        | consts::IGNCR
        | consts::ICRNL
        | consts::IXON);
    raw.c_oflag &= !consts::OPOST;
    raw.c_lflag &= !(consts::ECHO | consts::ECHONL | consts::ICANON | consts::ISIG | consts::IEXTEN);
    raw.c_cflag &= !(consts::CSIZE | consts::PARENB);
    raw.c_cflag |= consts::CS8;
    raw.c_cc[consts::VMIN] = 1;
    raw.c_cc[consts::VTIME] = 0;
    if unsafe { sys::tcsetattr(fd, consts::TCSANOW, &raw) } != 0 {
        return Err(sys::last_os_error("tcsetattr"));
    }
    *ORIG_TERMIOS.lock().unwrap() = Some(orig);
    Ok(RawMode)
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Ok(t) = ORIG_TERMIOS.lock() {
            if let Some(orig) = *t {
                unsafe { sys::tcsetattr(STDIN_FILENO, consts::TCSANOW, &orig) };
            }
        }
    }
}

fn fd_readable(fd: i32, timeout_ms: i32) -> bool {
    let mut pfd = PollFd {
        fd,
        events: consts::POLLIN,
        revents: 0,
    };
    (unsafe { sys::poll(&mut pfd, 1, timeout_ms) }) > 0
}

/// Read one byte directly from stdin (fd 0).
///
/// Unbuffered on purpose: `std::io::stdin()` fills an 8 KiB userspace buffer,
/// which would make `poll` on fd 0 miss bytes that are already in that buffer
/// (e.g. the rest of an escape sequence) — see the `\x1b[A` lookahead.
fn read_byte() -> io::Result<Option<u8>> {
    let mut b = [0u8; 1];
    let n = unsafe { sys::read(0, b.as_mut_ptr() as *mut c_void, 1) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if n == 0 {
        Ok(None)
    } else {
        Ok(Some(b[0]))
    }
}

/// 带超时读一个字节（用于转义序列的后续字节探测）；超时返回 None。
fn read_byte_timeout(ms: i32) -> io::Result<Option<u8>> {
    if !fd_readable(STDIN_FILENO, ms) {
        return Ok(None);
    }
    read_byte()
}

/// Read one key event. Blocks (polling the shutdown flag) until a key is
/// available.
pub fn read_key() -> io::Result<Option<KeyEvent>> {
    loop {
        if TERM_SHUTDOWN.load(Ordering::SeqCst) {
            return Ok(Some(KeyEvent {
                key: Key::Quit,
                raw: Vec::new(),
            }));
        }
        if fd_readable(STDIN_FILENO, 100) {
            break;
        }
    }
    let first = match read_byte()? {
        Some(b) => b,
        None => return Ok(None),
    };
    let mut raw = vec![first];
    let key = match first {
        0x03 => Key::Quit,                  // Ctrl-C：退出（不转发）
        0x00 => Key::Code(0x20, MOD_CTRL),  // Ctrl-Space（中英切换，交给 rime）
        0x09 => Key::Code(KEY_TAB, 0),
        0x0d | 0x0a => Key::Code(KEY_RETURN, 0),
        0x08 | 0x7f => Key::Code(KEY_BACKSPACE, 0),
        0x1b => return read_escape().map(|e| e.map(|(key, raw)| KeyEvent { key, raw })),
        c if c < 0x20 => Key::Code((c as i32) + 0x40, MOD_CTRL), // Ctrl-字母等组合键
        c if c < 0x7f => Key::Char(c as char),
        c => match read_utf8_char(c, &mut raw)? {
            Some(ch) => Key::Char(ch),
            None => return read_key(), // invalid UTF-8: skip and read the next key
        },
    };
    Ok(Some(KeyEvent { key, raw }))
}

fn read_utf8_char(first: u8, raw: &mut Vec<u8>) -> io::Result<Option<char>> {
    let len = match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Ok(None),
    };
    let mut buf = Vec::with_capacity(len);
    buf.push(first);
    for _ in 1..len {
        match read_byte()? {
            Some(b) => {
                raw.push(b);
                buf.push(b);
            }
            None => break,
        }
    }
    Ok(String::from_utf8(buf).ok().and_then(|s| s.chars().next()))
}

/// Parse an escape sequence after the leading `\x1b` byte.
/// 返回 (解码按键, 该事件消耗的全部原始字节)。
fn read_escape() -> io::Result<Option<(Key, Vec<u8>)>> {
    // Lone `\x1b` (nothing within 50ms) = Escape.
    let Some(b) = read_byte_timeout(50)? else {
        return Ok(Some((Key::Code(KEY_ESCAPE, 0), vec![0x1b])));
    };
    let mut raw = vec![0x1b, b];
    let key = match b {
        b'[' => {
            // CSI 序列：读到终结字节（0x40..=0x7e）为止，再整条匹配。
            loop {
                match read_byte_timeout(50)? {
                    Some(c) => {
                        raw.push(c);
                        if (0x40..=0x7e).contains(&c) {
                            break;
                        }
                    }
                    None => break, // 超时：序列不完整，按原样转发
                }
            }
            match raw.as_slice() {
                b"\x1b[A" => Key::Code(KEY_UP, 0),
                b"\x1b[B" => Key::Code(KEY_DOWN, 0),
                b"\x1b[C" => Key::Code(KEY_RIGHT, 0),
                b"\x1b[D" => Key::Code(KEY_LEFT, 0),
                b"\x1b[H" => Key::Code(KEY_HOME, 0),
                b"\x1b[F" => Key::Code(KEY_END, 0),
                b"\x1b[Z" => Key::Code(KEY_TAB, MOD_SHIFT),
                b"\x1b[1~" => Key::Code(KEY_HOME, 0),
                b"\x1b[3~" => Key::Code(KEY_DELETE, 0),
                b"\x1b[4~" => Key::Code(KEY_END, 0),
                b"\x1b[5~" => Key::Code(KEY_PAGE_UP, 0),
                b"\x1b[6~" => Key::Code(KEY_PAGE_DOWN, 0),
                b"\x1b[7~" => Key::Code(KEY_HOME, 0),
                b"\x1b[8~" => Key::Code(KEY_END, 0),
                // 未识别的 CSI：先尝试 kitty keyboard protocol 解析（现代终端/
                // tmux extended-keys 把组合键编码成 \x1b[27;<mod>;<code>~ 或
                // \x1b[<code>;<mod>u），成功则转发标准化后的传统字节。
                _ => match parse_kitty_sequence(&raw) {
                    Some((key, std_bytes)) => return Ok(Some((key, std_bytes))),
                    None => Key::Raw(raw.clone()),
                },
            }
        }
        b'O' => {
            // SS3 序列（部分终端的 \x1bOA 等方向键）。
            loop {
                match read_byte_timeout(50)? {
                    Some(c) => {
                        raw.push(c);
                        if (0x40..=0x7e).contains(&c) {
                            break;
                        }
                    }
                    None => break,
                }
            }
            match raw.as_slice() {
                b"\x1bOA" => Key::Code(KEY_UP, 0),
                b"\x1bOB" => Key::Code(KEY_DOWN, 0),
                b"\x1bOC" => Key::Code(KEY_RIGHT, 0),
                b"\x1bOD" => Key::Code(KEY_LEFT, 0),
                b"\x1bOH" => Key::Code(KEY_HOME, 0),
                b"\x1bOF" => Key::Code(KEY_END, 0),
                _ => Key::Raw(raw.clone()),
            }
        }
        // Alt+字符（\x1bX）：交给 rime 判断，未消费则原样转发
        c if (0x20..=0x7f).contains(&c) => Key::Code(c as i32, MOD_ALT),
        // 其他前缀（如 \x1b\x7f = Alt+退格）：原样转发
        _ => Key::Raw(raw.clone()),
    };
    Ok(Some((key, raw)))
}

// ---------------------------------------------------------------------------
// kitty keyboard protocol（tmux 3.4+ extended-keys、kitty、wezterm 等默认启用）
//
// 终端把组合键编码成 CSI 序列，而不是传统的单字节/传统 CSI：
//   \x1b[27;<mod>;<code>~   （通用形式，code 为 Unicode 码点或特殊键码）
//   \x1b[<code>;<mod>u      （CSI u 形式）
// mod 位掩码：1=Shift 2=Alt 4=Ctrl 8=Super 16=Hyper 32=Meta。
//
// 解析后转成 (keysym, librime mask)，并把转发字节标准化为传统终端形式
// （如 Ctrl+R → 0x12），这样目标 pane 里的程序（bash/vim 等）能正确识别。
// ---------------------------------------------------------------------------

/// 解析一条 kitty keyboard protocol 序列，返回 (按键, 标准化转发字节)。
fn parse_kitty_sequence(raw: &[u8]) -> Option<(Key, Vec<u8>)> {
    let s = raw.strip_prefix(b"\x1b[")?;
    let (term, params) = s.split_last()?; // split_last 返回 (最后一个元素, 其余部分)
    if *term != b'~' && *term != b'u' {
        return None;
    }
    let mut nums = Vec::new();
    for p in params.split(|&b| b == b';') {
        nums.push(std::str::from_utf8(p).ok()?.parse::<i64>().ok()?);
    }
    // 通用形式 \x1b[27;<mod>;<code>~ ；CSI u 形式 \x1b[<code>;<mod>u
    let (code, mod_bits) = if nums.len() == 3 && nums[0] == 27 && *term == b'~' {
        (nums[2], nums[1])
    } else if nums.len() >= 2 && *term == b'u' {
        (nums[0], nums[1])
    } else {
        return None;
    };
    // 只处理 Shift/Alt/Ctrl（传统终端字节无法表达 Super/Hyper/Meta）
    if mod_bits & !(1 | 2 | 4) != 0 {
        return None;
    }
    let keysym = kitty_code_to_keysym(code)?;
    let mask = kitty_mod_to_librime(mod_bits);
    let std_bytes = standard_bytes(keysym, mask);
    Some((Key::Code(keysym, mask), std_bytes))
}

/// kitty 键码 → X11 keysym（与 lua/rime/key.lua 一致）。
fn kitty_code_to_keysym(code: i64) -> Option<i32> {
    if code > 0 && code < 256 {
        // 普通字符：字母转大写，与单字节 Ctrl+字母 路径（0x12 → 'R'）保持一致
        let c = code as i32;
        return Some(if (0x61..=0x7a).contains(&c) { c - 0x20 } else { c });
    }
    // kitty 特殊键码（私用区 57344 起，见 kitty keyboard protocol 文档）
    let k = match code {
        57344 => KEY_ESCAPE,
        57345 => KEY_RETURN,
        57346 => KEY_TAB,
        57347 => KEY_BACKSPACE,
        57348 => 0xff63, // Insert
        57349 => KEY_DELETE,
        57350 => KEY_LEFT,
        57351 => KEY_RIGHT,
        57352 => KEY_UP,
        57353 => KEY_DOWN,
        57354 => KEY_PAGE_UP,
        57355 => KEY_PAGE_DOWN,
        57356 => KEY_HOME,
        57357 => KEY_END,
        57364..=57375 => 65470 + (code - 57364) as i32, // F1..F12 (XK_F1=0xffbe)
        _ => return None,
    };
    Some(k)
}

/// kitty 修饰位 → librime 修饰掩码（S=1, C=4, A=8）。
fn kitty_mod_to_librime(mod_bits: i64) -> i32 {
    let mut mask = 0;
    if mod_bits & 1 != 0 {
        mask |= MOD_SHIFT;
    }
    if mod_bits & 2 != 0 {
        mask |= MOD_ALT;
    }
    if mod_bits & 4 != 0 {
        mask |= MOD_CTRL;
    }
    mask
}

/// 把 (keysym, mask) 编码为传统终端字节（转发给目标 pane 用）。
fn standard_bytes(code: i32, mask: i32) -> Vec<u8> {
    match code {
        KEY_UP => return csi_key(b'A', None, mask),
        KEY_DOWN => return csi_key(b'B', None, mask),
        KEY_RIGHT => return csi_key(b'C', None, mask),
        KEY_LEFT => return csi_key(b'D', None, mask),
        KEY_HOME => return csi_key(0, Some(1), mask),
        KEY_END => return csi_key(0, Some(4), mask),
        KEY_PAGE_UP => return csi_key(0, Some(5), mask),
        KEY_PAGE_DOWN => return csi_key(0, Some(6), mask),
        KEY_DELETE => return csi_key(0, Some(3), mask),
        KEY_INSERT => return csi_key(0, Some(2), mask),
        KEY_BACKSPACE => return vec![0x7f],
        KEY_TAB => return if mask & MOD_SHIFT != 0 { b"\x1b[Z".to_vec() } else { vec![0x09] },
        KEY_RETURN => return vec![0x0d],
        KEY_ESCAPE => return vec![0x1b],
        _ => {}
    }
    let mut v = Vec::new();
    if mask & MOD_ALT != 0 {
        v.push(0x1b);
    }
    let c = if mask & MOD_CTRL != 0 {
        (code & 0x1f) as u8 // Ctrl+字母/标点 → 单字节控制符
    } else {
        let mut c = code as u8;
        if mask & MOD_SHIFT != 0 && (0x61..=0x7a).contains(&code) {
            c -= 0x20; // 大写
        }
        c
    };
    v.push(c);
    v
}

/// 传统 CSI 按键：箭头键 \x1b[1;{mod}A，或 \x1b[{n};{mod}~ 形式。
fn csi_key(letter: u8, tilde: Option<u8>, mask: i32) -> Vec<u8> {
    let m = trad_mod(mask);
    if let Some(n) = tilde {
        if m == 0 {
            format!("\x1b[{}~", n).into_bytes()
        } else {
            format!("\x1b[{};{}~", n, m).into_bytes()
        }
    } else if m == 0 {
        vec![0x1b, b'[', letter]
    } else {
        format!("\x1b[1;{}{}", m, letter as char).into_bytes()
    }
}

/// librime mask → 传统 CSI 修饰数字（1=Shift 2=Alt 4=Ctrl，组合相加）。
fn trad_mod(mask: i32) -> i32 {
    let mut m = 0;
    if mask & MOD_SHIFT != 0 {
        m += 1;
    }
    if mask & MOD_ALT != 0 {
        m += 2;
    }
    if mask & MOD_CTRL != 0 {
        m += 4;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_ctrl_letter() {
        // Ctrl+R: \x1b[27;4;114~ → 0x12；Ctrl+D → 0x04
        let (key, bytes) = parse_kitty_sequence(b"\x1b[27;4;114~").unwrap();
        assert_eq!(key, Key::Code(0x52, MOD_CTRL));
        assert_eq!(bytes, b"\x12");
        let (key, bytes) = parse_kitty_sequence(b"\x1b[27;4;100~").unwrap();
        assert_eq!(key, Key::Code(0x44, MOD_CTRL));
        assert_eq!(bytes, b"\x04");
    }

    #[test]
    fn kitty_csi_u_and_modifiers() {
        // CSI u 形式：\x1b[114;4u = Ctrl+R
        let (key, bytes) = parse_kitty_sequence(b"\x1b[114;4u").unwrap();
        assert_eq!(key, Key::Code(0x52, MOD_CTRL));
        assert_eq!(bytes, b"\x12");
        // Ctrl+Shift+A: mod=5
        let (key, bytes) = parse_kitty_sequence(b"\x1b[27;5;97~").unwrap();
        assert_eq!(key, Key::Code(0x41, MOD_SHIFT | MOD_CTRL));
        assert_eq!(bytes, b"\x01");
        // Alt+X: mod=2
        let (key, bytes) = parse_kitty_sequence(b"\x1b[27;2;120~").unwrap();
        assert_eq!(key, Key::Code(0x58, MOD_ALT));
        assert_eq!(bytes, b"\x1bX");
    }

    #[test]
    fn kitty_special_keys() {
        // Ctrl+Up: kitty 码 57352
        let (key, bytes) = parse_kitty_sequence(b"\x1b[27;4;57352~").unwrap();
        assert_eq!(key, Key::Code(KEY_UP, MOD_CTRL));
        assert_eq!(bytes, b"\x1b[1;4A");
        // 无修饰 Home: 57356
        let (key, bytes) = parse_kitty_sequence(b"\x1b[27;0;57356~").unwrap();
        assert_eq!(key, Key::Code(KEY_HOME, 0));
        assert_eq!(bytes, b"\x1b[1~");
    }

    #[test]
    fn kitty_rejects_traditional_and_unknown() {
        // 传统 CSI 不是 kitty 序列
        assert!(parse_kitty_sequence(b"\x1b[1;5A").is_none());
        // Super 修饰（8）无法用传统字节表达 → 不解析
        assert!(parse_kitty_sequence(b"\x1b[27;8;114~").is_none());
    }
}
