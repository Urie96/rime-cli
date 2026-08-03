//! Minimal platform FFI for raw terminal mode.
//!
//! Avoids the `libc` crate entirely: on macOS it emits `-liconv`, which this
//! environment's linker cannot resolve (the same reason rime-daemon declares
//! `signal` by hand). Only the handful of symbols we need are declared here,
//! all of which live in libSystem (macOS) / libc (Linux) and link by default.

use std::ffi::c_void;
use std::io;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub c_iflag: u64,
    pub c_oflag: u64,
    pub c_cflag: u64,
    pub c_lflag: u64,
    pub c_cc: [u8; 20],
    pub c_ispeed: u64,
    pub c_ospeed: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 32],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

// Android (bionic) 使用内核布局的 `struct termios`：NCCS=19，且没有
// c_ispeed/c_ospeed 尾随字段（glibc 才有）。tcgetattr/tcsetattr 只读写
// 前 36 字节，其余标志常量与 Linux 模块的值一致（见下方 consts）。
#[cfg(target_os = "android")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 19],
}

#[cfg(target_os = "macos")]
pub mod consts {
    pub const TCSANOW: i32 = 0;
    pub const VMIN: usize = 16;
    pub const VTIME: usize = 17;
    pub const IGNBRK: u64 = 0x0000_0001;
    pub const BRKINT: u64 = 0x0000_0002;
    pub const PARMRK: u64 = 0x0000_0008;
    pub const ISTRIP: u64 = 0x0000_0020;
    pub const INLCR: u64 = 0x0000_0040;
    pub const IGNCR: u64 = 0x0000_0080;
    pub const ICRNL: u64 = 0x0000_0100;
    pub const IXON: u64 = 0x0000_0200;
    pub const OPOST: u64 = 0x0000_0001;
    pub const ECHO: u64 = 0x0000_0008;
    pub const ECHONL: u64 = 0x0000_0010;
    pub const ICANON: u64 = 0x0000_0100;
    pub const ISIG: u64 = 0x0000_0080;
    pub const IEXTEN: u64 = 0x0000_0400;
    pub const CSIZE: u64 = 0x0000_0300;
    pub const CS8: u64 = 0x0000_0300;
    pub const PARENB: u64 = 0x0000_1000;
    pub const POLLIN: i16 = 0x0001;
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub mod consts {
    pub const TCSANOW: i32 = 0;
    pub const VMIN: usize = 6;
    pub const VTIME: usize = 5;
    pub const IGNBRK: u32 = 0x0000_0001;
    pub const BRKINT: u32 = 0x0000_0002;
    pub const PARMRK: u32 = 0x0000_0008;
    pub const ISTRIP: u32 = 0x0000_0010;
    pub const INLCR: u32 = 0x0000_0020;
    pub const IGNCR: u32 = 0x0000_0040;
    pub const ICRNL: u32 = 0x0000_0080;
    pub const IXON: u32 = 0x0000_0400;
    pub const OPOST: u32 = 0x0000_0001;
    pub const ECHO: u32 = 0x0000_0008;
    pub const ECHONL: u32 = 0x0000_0040;
    pub const ICANON: u32 = 0x0000_0002;
    pub const ISIG: u32 = 0x0000_0001;
    pub const IEXTEN: u32 = 0x0000_8000;
    pub const CSIZE: u32 = 0x0000_0030;
    pub const CS8: u32 = 0x0000_0030;
    pub const PARENB: u32 = 0x0000_0100;
    pub const POLLIN: i16 = 0x0001;
}

unsafe extern "C" {
    pub fn tcgetattr(fd: i32, termios_p: *mut Termios) -> i32;
    pub fn tcsetattr(fd: i32, optional_actions: i32, termios_p: *const Termios) -> i32;
    pub fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32;
    pub fn setsid() -> i32;
    pub fn read(fd: i32, buf: *mut c_void, count: usize) -> isize;
}

// macOS 标准信号编号（两个平台一致：SIGINT=2, SIGTERM=15）。
pub const SIGTERM: i32 = 15;
pub const SIGINT: i32 = 2;

type SigHandler = extern "C" fn(i32);

unsafe extern "C" {
    #[link_name = "signal"]
    pub fn c_signal(signum: i32, handler: SigHandler) -> SigHandler;
}

/// Convenience wrapper returning the last OS error as a string.
pub fn last_os_error(what: &str) -> String {
    format!("{what}: {}", io::Error::last_os_error())
}
