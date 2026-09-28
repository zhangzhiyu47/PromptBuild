//! Rendering for the two-line shell prompt.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Color constants.
const RESET: &str = "\x1b[0m";
const BORDER_ROOT: &str = "\x1b[34m"; // Blue
const BORDER_USER: &str = "\x1b[32m"; // Green
const USER_ROOT: &str = "\x1b[1;31m"; // Red
const USER_NORMAL: &str = "\x1b[1;34m"; // Blue

/// User permission identifier
const PROMPT_ROOT: &str = "#";
const PROMPT_USER: &str = "$";

/// Border
const LINK: &str = "─";

/// Brackets around a segment.
#[derive(Clone, Copy)]
pub enum Frame {
    Paren,
    Bracket,
    #[allow(dead_code)]
    None,
}

impl Frame {
    fn brackets(self) -> Option<(char, char)> {
        match self {
            Frame::Paren => Some(('(', ')')),
            Frame::Bracket => Some(('[', ']')),
            Frame::None => None,
        }
    }
}

/// Content colour of a segment.
#[derive(Clone, Copy)]
pub enum Color {
    Reset,
    BoldRed,
    #[allow(dead_code)]
    Green,
    #[allow(dead_code)]
    Blue,
    BoldBlue,
    Yellow,
    /// Follow the border colour chosen by `render_prompt`.
    Border,
}

impl Color {
    fn code(self, border: &'static str) -> &'static str {
        match self {
            Color::Reset => "\x1b[0m",
            Color::BoldRed => "\x1b[1;31m",
            Color::Green => "\x1b[32m",
            Color::Blue => "\x1b[34m",
            Color::BoldBlue => "\x1b[1;34m",
            Color::Yellow => "\x1b[33m",
            Color::Border => border,
        }
    }
}

/// A framed piece of the first prompt line:
/// frame, content, content colour. Always trailed by `LINK`.
pub struct Segment {
    pub frame: Frame,
    pub content: String,
    pub color: Color,
}

impl Segment {
    /// Visible width: brackets + content + link.
    fn width(&self) -> usize {
        let frame = self
            .frame
            .brackets()
            .map_or(0, |(o, c)| o.len_utf8() + c.len_utf8());
        frame + UnicodeWidthStr::width(self.content.as_str()) + UnicodeWidthStr::width(LINK)
    }

    /// Render with ANSI colours, ending in the link.
    fn render(&self, border: &'static str) -> String {
        let color = self.color.code(border);
        let body = match self.frame.brackets() {
            Some((open, close)) => format!(
                "{border}{open}{color}{content}{border}{close}",
                content = self.content,
            ),
            None => format!("{color}{}", self.content),
        };
        format!("{body}{border}{LINK}{RESET}")
    }
}

/// Working directory and how to render it.
pub struct PathDisplay {
    pub text: String,
    pub color: Color,
}

/// Clip `s` to `budget` columns from the front.
fn clip_front(s: &str, budget: usize) -> &str {
    let mut width = 0;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        let w = c.width().unwrap_or(0);
        if width + w > budget {
            break;
        }
        width += w;
        end = i + c.len_utf8();
    }
    &s[..end]
}

/// Clip `s` to `budget` columns from the front, backing up to the last '/'.
fn clip_head(s: &str, budget: usize) -> &str {
    let clipped = clip_front(s, budget);
    match clipped.rfind('/') {
        Some(p) if p > 0 => &clipped[..p],
        _ => clipped,
    }
}

/// Clip `s` to the last `budget` columns, keeping only complete
/// trailing segments (starting right after a '/').
fn clip_tail(s: &str, budget: usize) -> &str {
    let mut width = 0;
    let mut after_slash = None;
    for (i, c) in s.char_indices().rev() {
        let w = c.width().unwrap_or(0);
        if width + w > budget {
            break;
        }
        width += w;
        if c == '/' {
            after_slash = Some(i + 1);
        }
    }
    after_slash.map_or("", |i| &s[i..])
}

/// Clip `s` to the last `budget` columns.
fn clip_back(s: &str, budget: usize) -> &str {
    let mut width = 0;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        let w = c.width().unwrap_or(0);
        if width + w > budget {
            break;
        }
        width += w;
        start = i;
    }
    &s[start..]
}

/// Turn a long path into an abbreviated form: keep as many trailing
/// complete segments as fit, then fill the remaining space with the head.
fn truncate_middle(path: &str, max: usize) -> String {
    if UnicodeWidthStr::width(path) <= max {
        return path.to_string();
    }

    const ELLIPSIS: &str = "…";
    let ell_width = UnicodeWidthStr::width(ELLIPSIS);
    if max <= ell_width {
        return ELLIPSIS.to_string();
    }

    let last_start = path.rfind('/').map_or(0, |p| p + 1);
    let last = &path[last_start..];
    let last_width = UnicodeWidthStr::width(last);

    let connector = ell_width + 2; // "/…/"
    if max >= connector {
        let available = max - connector;
        let tail = clip_tail(path, available);
        let tail_width = UnicodeWidthStr::width(tail);

        if !tail.is_empty() && tail_width >= last_width {
            let head_budget = available - tail_width;
            let head = clip_head(path, head_budget);
            let head = if head.is_empty() || head == "/" {
                ""
            } else {
                head
            };

            return if head.is_empty() {
                format!("{ELLIPSIS}/{tail}")
            } else {
                format!("{head}/{ELLIPSIS}/{tail}")
            };
        }
    }

    if ell_width + 1 + last_width <= max {
        return format!("{ELLIPSIS}/{last}");
    }
    format!("{ELLIPSIS}{}", clip_back(last, max - ell_width))
}

/// Render the prompt: segments flow across up to two content lines,
/// the path trails the last one, and the final line is always `└─$`/`└─#`.
pub fn render_prompt(cols: usize, segments: &[Segment], is_root: bool, path: &PathDisplay) {
    let (border, user_color, sym) = if is_root {
        (BORDER_ROOT, USER_ROOT, PROMPT_ROOT)
    } else {
        (BORDER_USER, USER_NORMAL, PROMPT_USER)
    };

    // Same width (3 columns) for every prefix, so one budget serves both.
    const P_FIRST: &str = "┌──";
    const P_FIRST_SPLIT: &str = "┌┬─";
    const P_SECOND: &str = "│└─";
    const P_LAST: &str = "└─";
    const PATH_WRAP: usize = 2; // "[" + "]"

    let budget = cols.saturating_sub(UnicodeWidthStr::width(P_FIRST));

    let seg_renders: Vec<String> = segments.iter().map(|s| s.render(border)).collect();
    let seg_w: Vec<usize> = segments.iter().map(|s| s.width()).collect();

    let mut line0 = String::new();
    let mut line1 = String::new();
    let mut w0 = 0;
    let mut w1 = 0;
    let mut split = false;
    let mut overflow = false;

    for (render, &w) in seg_renders.iter().zip(seg_w.iter()) {
        if !split && w0 + w <= budget {
            line0.push_str(render);
            w0 += w;
        } else if w1 + w <= budget {
            split = true;
            line1.push_str(render);
            w1 += w;
        } else {
            overflow = true;
            break;
        }
    }

    if split && !line0.is_empty() {
        let dangling = format!("{border}{LINK}{RESET}");
        if let Some(rest) = line0.strip_suffix(&dangling) {
            line0.truncate(rest.len());
        }
    }

    let (last_line, last_w) = if split {
        (&mut line1, w1)
    } else {
        (&mut line0, w0)
    };

    if overflow {
        last_line.push_str(&format!("{border}...{RESET}"));
    } else {
        let avail = budget.saturating_sub(last_w).saturating_sub(PATH_WRAP);
        if avail > 0 {
            let path_text = truncate_middle(&path.text, avail);
            last_line.push_str(&format!(
                "{border}[{color}{path_text}{border}]",
                color = path.color.code(border),
            ));
        }
    }

    let first = if split { P_FIRST_SPLIT } else { P_FIRST };
    println!();
    println!("{border}{first}{line0}{RESET}");
    if split {
        println!("{border}{P_SECOND}{line1}{RESET}");
    }
    print!("{border}{P_LAST}{user_color}{sym}{RESET} ");
}
