//! 2-line display state + ANSI rendering.
//!
//! Line 1: preedit（光标用终端真实光标表示，见 render 末尾）
//! Line 2: 候选词（带序号与注释）
//!
//! 已上屏文本不再由 CLI 维护：rime 的上屏文本与未被消费的按键都实时转发到
//! stdout（数据通道，可接入 tmux pane 等），界面只负责展示 preedit/候选。

use crate::client::Client;

#[derive(Default)]
pub struct State {
    /// 当前 preedit（拼音串等）。
    pub preedit: String,
    /// preedit 内光标位置（字符下标）。
    pub cursor: usize,
    /// (候选词, 注释)。
    pub candidates: Vec<(String, String)>,
    /// 高亮候选的下标；-1 表示无高亮。
    pub highlighted: i64,
    /// 当前页是否还有更多候选（末尾显示 …）。
    pub has_more: bool,
    /// 当前方案显示名（无 preedit 时显示在方括号里）。
    pub schema: Option<String>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_preedit(&self) -> bool {
        !self.preedit.is_empty()
    }

    /// 从 daemon 拉取最新 commit 与 context。
    /// 返回本次新上屏的文本（空串 = 无），由调用方转发到 stdout。
    pub fn refresh(&mut self, client: &mut Client) -> String {
        let text = client.get_commit().unwrap_or_default();
        match client.get_context() {
            Ok(ctx) => {
                let comp = &ctx["composition"];
                self.preedit = comp["preedit"].as_str().unwrap_or("").to_string();
                self.cursor = comp["cursor_pos"].as_i64().unwrap_or(0).max(0) as usize;
                let menu = &ctx["menu"];
                self.candidates = menu["candidates"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|c| {
                                (
                                    c["text"].as_str().unwrap_or("").to_string(),
                                    c["comment"].as_str().unwrap_or("").to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.highlighted = menu["highlighted_candidate_index"].as_i64().unwrap_or(0);
                self.has_more = !menu["is_last_page"].as_bool().unwrap_or(true);
            }
            Err(_) => {
                self.preedit.clear();
                self.candidates.clear();
                self.cursor = 0;
                self.highlighted = -1;
                self.has_more = false;
            }
        }
        text
    }
}

/// 单个字符在终端中的显示宽度：0（零宽/组合）、1、2（CJK/全角）。
fn char_width(c: char) -> usize {
    let cp = c as u32;
    // 零宽字符（组合音标、变体选择符等）
    if (0x0300..=0x036f).contains(&cp)
        || (0x1ab0..=0x1aff).contains(&cp)
        || (0x1dc0..=0x1dff).contains(&cp)
        || (0x20d0..=0x20ff).contains(&cp)
        || (0xfe00..=0xfe0f).contains(&cp)
        || (0xfe20..=0xfe2f).contains(&cp)
    {
        return 0;
    }
    // 宽字符（CJK / 全角 / 谚文 / 表意文字 / emoji）
    if (0x1100..=0x115f).contains(&cp) // 谚文 Jamo
        || (0x2e80..=0x303e).contains(&cp) // CJK 部首/康熙/符号
        || (0x3041..=0x33ff).contains(&cp) // 假名 / CJK 兼容
        || (0x3400..=0x4dbf).contains(&cp) // CJK 扩展 A
        || (0x4e00..=0x9fff).contains(&cp) // CJK 统一表意
        || (0xa000..=0xa4cf).contains(&cp) // 彝文
        || (0xa960..=0xa97f).contains(&cp) // 谚文 Jamo 扩展 A
        || (0xac00..=0xd7a3).contains(&cp) // 谚文音节
        || (0xf900..=0xfaff).contains(&cp) // CJK 兼容表意
        || (0xfe30..=0xfe4f).contains(&cp) // CJK 兼容形式
        || (0xff00..=0xff60).contains(&cp) // 全角形式
        || (0xffe0..=0xffe6).contains(&cp) // 全角符号
        || (0x1f300..=0x1faff).contains(&cp) // emoji（终端通常按双宽）
        || (0x20000..=0x2fffd).contains(&cp) // CJK 扩展 B–F
        || (0x30000..=0x3fffd).contains(&cp) // CJK 扩展 G+（含 U+3FFFD 未分配区，无妨）
    {
        2
    } else {
        1
    }
}

/// preedit 前 n 个字符在终端中的显示宽度（列偏移用）。
fn display_width(s: &str, n: usize) -> usize {
    s.chars().take(n).map(char_width).sum()
}

/// Render the 2-line frame (full clear + home + 2 lines).
/// 光标不再手画 `|`：有 preedit 时把终端真实光标移到 preedit 光标处并以
/// 竖线（DECSCUSR bar）显示；空闲时隐藏光标。
pub fn render(s: &State) -> String {
    let mut out = String::new();
    out.push_str("\x1b[2J\x1b[H");

    // 第 1 行：preedit（空闲时显示方案名占位）
    out.push_str("\x1b[2K");
    if s.preedit.is_empty() {
        match s.schema.as_deref() {
            Some(name) if !name.is_empty() => {
                out.push_str("\x1b[90m[");
                out.push_str(name);
                out.push_str("]\x1b[0m");
            }
            _ => out.push_str("\x1b[90m…\x1b[0m"),
        }
    } else {
        out.push_str("\x1b[36m");
        out.push_str(&s.preedit);
        out.push_str("\x1b[0m");
    }
    out.push_str("\r\n");

    // 第 2 行：候选词
    out.push_str("\x1b[2K");
    for (i, (text, comment)) in s.candidates.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        let highlighted = s.highlighted >= 0 && i as i64 == s.highlighted;
        if highlighted {
            out.push_str("\x1b[7m");
        }
        out.push_str("\x1b[33m");
        out.push_str(&(i + 1).to_string());
        out.push('.');
        out.push_str("\x1b[0m");
        out.push(' ');
        out.push_str(text);
        if !comment.is_empty() {
            out.push(' ');
            out.push_str("\x1b[2m");
            out.push_str(comment);
            out.push_str("\x1b[0m");
        }
        if highlighted {
            out.push_str("\x1b[0m");
        }
    }
    if s.has_more {
        out.push_str("\x1b[90m …\x1b[0m");
    }
    out.push_str("\x1b[0m");

    // 光标：有 preedit 时移到第 1 行 preedit 光标处（列 = 前部显示宽度 + 1），
    // 并设为竖线光标（`\x1b[5 q` = 闪烁竖线，同 vim 插入态）；空闲时隐藏。
    if !s.preedit.is_empty() {
        let col = 1 + display_width(&s.preedit, s.cursor);
        out.push_str(&format!("\x1b[1;{col}H\x1b[?25h\x1b[5 q"));
    } else {
        out.push_str("\x1b[?25l");
    }
    out
}
