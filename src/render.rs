//! Rendering for the two-line shell prompt.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Border
const LINK: &str = "─";

/// Brackets around a segment.
#[derive(Clone, Copy)]
pub enum Frame {
    Paren,
    Bracket,
    //None,
}

impl Frame {
    fn brackets(self) -> Option<(char, char)> {
        match self {
            Frame::Paren => Some(('(', ')')),
            Frame::Bracket => Some(('[', ']')),
            //Frame::None => None,
        }
    }
}

/// Content colour of a segment.
#[derive(Clone, Copy)]
pub enum Color {
    None,
    BoldRed,
    BoldBlue,
    Yellow,
    Gray,
    Border,
}

fn is_no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

impl Color {
    fn code(self, border: &'static str, no_color: bool) -> &'static str {
        if no_color {
            return "";
        }

        match self {
            Color::None => "\x1b[0m",
            Color::BoldRed => "\x1b[1;31m",
            Color::BoldBlue => "\x1b[1;34m",
            Color::Yellow => "\x1b[33m",
            Color::Gray => "\x1b[90m",
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
    fn render(&self, border: &'static str, no_color: bool, reset: &'static str) -> String {
        let color = self.color.code(border, no_color);
        let body = match self.frame.brackets() {
            Some((open, close)) => format!(
                "{border}{open}{color}{content}{border}{close}",
                content = self.content,
            ),
            None => format!("{color}{}", self.content),
        };
        format!("{body}{border}{LINK}{reset}")
    }
}

/// Everything needed to render one prompt.
pub struct Prompt {
    // Layout
    pub cols: usize,
    pub blank_lines: usize,

    // Content
    pub segments: Vec<Segment>,
    pub is_root: bool,

    // Path
    pub path_text: String,
    pub path_color: Color,
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

/// Render the prompt to stdout.
///
/// Segments flow across up to two lines; the path trails the last
/// line. If the segments don't fit, or there's no room for the path,
/// fall back to a bare `$`/`#`.
pub fn render_prompt(prompt: Prompt) {
    let no_color = is_no_color();

    let sym = if prompt.is_root { "#" } else { "$" };

    // Color constants.
    const RESET: &str = "\x1b[0m";
    const BORDER_ROOT: &str = "\x1b[34m"; // Blue
    const BORDER_USER: &str = "\x1b[32m"; // Green
    const USER_ROOT: &str = "\x1b[1;31m"; // Red
    const USER_NORMAL: &str = "\x1b[1;34m"; // Blue

    let (border, user_color, reset) = if no_color {
        ("", "", "")
    } else if prompt.is_root {
        (BORDER_ROOT, USER_ROOT, RESET)
    } else {
        (BORDER_USER, USER_NORMAL, RESET)
    };

    // Same width (3 columns) for every prefix, so one budget serves both.
    const P_FIRST: &str = "┌──";
    const P_FIRST_SPLIT: &str = "┌┬─";
    const P_SECOND: &str = "│└─";
    const P_LAST: &str = "└─";
    const PATH_WRAP: usize = 2; // "[" + "]"

    let budget = prompt.cols.saturating_sub(UnicodeWidthStr::width(P_FIRST));

    let seg_renders: Vec<String> = prompt
        .segments
        .iter()
        .map(|s| s.render(border, no_color, reset))
        .collect();
    let seg_w: Vec<usize> = prompt.segments.iter().map(|s| s.width()).collect();

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
        let dangling = format!("{border}{LINK}{reset}");
        if let Some(rest) = line0.strip_suffix(&dangling) {
            line0.truncate(rest.len());
        }
    }

    let last_w = if split { w1 } else { w0 };
    let avail = budget.saturating_sub(last_w).saturating_sub(PATH_WRAP);
    if overflow || (!prompt.path_text.is_empty() && avail == 0) {
        for _ in 0..prompt.blank_lines {
            println!();
        }
        print!("{user_color}{sym}{reset} ");
        return;
    }

    let last_line = if split { &mut line1 } else { &mut line0 };
    if avail > 0 {
        let path_text = truncate_middle(&prompt.path_text, avail);
        last_line.push_str(&format!(
            "{border}[{color}{path_text}{border}]",
            color = prompt.path_color.code(border, no_color),
        ));
    }

    let first = if split { P_FIRST_SPLIT } else { P_FIRST };
    for _ in 0..prompt.blank_lines {
        println!();
    }
    println!("{border}{first}{line0}{reset}");
    if split {
        println!("{border}{P_SECOND}{line1}{reset}");
    }
    print!("{border}{P_LAST}{user_color}{sym}{reset} ");
}
