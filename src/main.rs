//! gitdif — show the changed lines of a file, plus its first and last N lines,
//! in a LazyVim-looking terminal UI.

use std::{
    cmp::Ordering,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::supports_keyboard_enhancement,
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Theme, ThemeSet},
    parsing::{SyntaxReference, SyntaxSet},
};

// ── Tokyonight palette ──────────────────────────────────────────────────────

const BG: Color = Color::Rgb(0x1a, 0x1b, 0x26);
const BG_DARK: Color = Color::Rgb(0x16, 0x16, 0x1e);
const BG_HL: Color = Color::Rgb(0x29, 0x2e, 0x42);
const BG_FOLD: Color = Color::Rgb(0x1f, 0x23, 0x35);
const FG: Color = Color::Rgb(0xc0, 0xca, 0xf5);
const FG_DARK: Color = Color::Rgb(0xa9, 0xb1, 0xd6);
const COMMENT: Color = Color::Rgb(0x56, 0x5f, 0x89);
const GUTTER: Color = Color::Rgb(0x3b, 0x42, 0x61);
const DARK3: Color = Color::Rgb(0x54, 0x5c, 0x7e);
const BLUE: Color = Color::Rgb(0x7a, 0xa2, 0xf7);
const CYAN: Color = Color::Rgb(0x7d, 0xcf, 0xff);
const GREEN: Color = Color::Rgb(0x9e, 0xce, 0x6a);
const RED: Color = Color::Rgb(0xf7, 0x76, 0x8e);
const ORANGE: Color = Color::Rgb(0xff, 0x9e, 0x64);
const YELLOW: Color = Color::Rgb(0xe0, 0xaf, 0x68);
const MAGENTA: Color = Color::Rgb(0xbb, 0x9a, 0xf7);
const ADD_BG: Color = Color::Rgb(0x20, 0x30, 0x3b);
const ADD_CUR: Color = Color::Rgb(0x2b, 0x42, 0x52);
const DEL_BG: Color = Color::Rgb(0x37, 0x22, 0x2c);
const DEL_CUR: Color = Color::Rgb(0x4b, 0x2a, 0x37);

const LOGO: &[&str] = &[
    " ██████╗ ██╗████████╗██████╗ ██╗███████╗",
    "██╔════╝ ██║╚══██╔══╝██╔══██╗██║██╔════╝",
    "██║  ███╗██║   ██║   ██║  ██║██║█████╗  ",
    "██║   ██║██║   ██║   ██║  ██║██║██╔══╝  ",
    "╚██████╔╝██║   ██║   ██████╔╝██║██║     ",
    " ╚═════╝ ╚═╝   ╚═╝   ╚═════╝ ╚═╝╚═╝     ",
];

const USAGE: &str = "\
gitdif — show changed lines plus the top/bottom of a file, LazyVim style

USAGE:
    gitdif [OPTIONS] [PATH]

    PATH is relative to the current directory. Without PATH, every changed
    file in the repository is listed in the explorer.

OPTIONS:
    -n, --lines <N>     lines to show at top and bottom of the file [default: 20]
    -C, --context <N>   context lines around each change [default: 0]
    -b, --base <REV>    revision to diff against [default: HEAD]
    -p, --print         print to stdout instead of opening the TUI
    -h, --help          show this help
";

// ── Config / args ───────────────────────────────────────────────────────────

struct Cfg {
    edge: usize,
    ctx: usize,
    base: String,
    print: bool,
    path: Option<String>,
}

fn parse_args() -> Result<Cfg> {
    let mut cfg = Cfg { edge: 20, ctx: 0, base: "HEAD".into(), print: false, path: None };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut num = |name: &str| -> Result<usize> {
            args.next()
                .with_context(|| format!("{name} needs a value"))?
                .parse()
                .with_context(|| format!("{name} expects a number"))
        };
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-n" | "--lines" => cfg.edge = num(&a)?,
            "-C" | "--context" => cfg.ctx = num(&a)?,
            "-b" | "--base" => cfg.base = args.next().context("--base needs a value")?,
            "-p" | "--print" => cfg.print = true,
            s if s.starts_with('-') && s.len() > 1 => bail!("unknown option: {s}\n\n{USAGE}"),
            _ if cfg.path.is_some() => bail!("only one PATH may be given"),
            _ => cfg.path = Some(a),
        }
    }
    Ok(cfg)
}

// ── Git ─────────────────────────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The empty tree, used as base in repositories without commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn resolve_base(root: &Path, base: &str) -> String {
    let spec = format!("{base}^{{commit}}");
    if git(root, &["rev-parse", "--verify", "-q", &spec]).is_ok() {
        base.to_string()
    } else {
        EMPTY_TREE.to_string()
    }
}

fn branch(root: &Path) -> String {
    git(root, &["symbolic-ref", "--short", "HEAD"])
        .or_else(|_| git(root, &["rev-parse", "--short", "HEAD"]))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "detached".into())
}

/// Changed files (relative to repo root) with a one-letter status.
fn changed_files(root: &Path, base: &str) -> Vec<(String, char)> {
    let mut files = Vec::new();
    if let Ok(out) = git(root, &["diff", "--name-status", "-z", "--no-renames", base]) {
        let mut it = out.split('\0').filter(|s| !s.is_empty());
        while let (Some(st), Some(path)) = (it.next(), it.next()) {
            files.push((path.to_string(), st.chars().next().unwrap_or('M')));
        }
    }
    if let Ok(out) = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]) {
        files.extend(out.split('\0').filter(|s| !s.is_empty()).map(|p| (p.to_string(), '?')));
    }
    files
}

struct RawHunk {
    old_start: usize,
    new_start: usize,
    new_count: usize,
    del: Vec<String>,
}

fn parse_range(s: &str) -> (usize, usize) {
    match s.split_once(',') {
        Some((a, b)) => (a.parse().unwrap_or(0), b.parse().unwrap_or(0)),
        None => (s.parse().unwrap_or(0), 1),
    }
}

fn parse_diff(out: &str) -> Vec<RawHunk> {
    let mut hunks: Vec<RawHunk> = Vec::new();
    for l in out.lines() {
        if let Some(rest) = l.strip_prefix("@@ ") {
            let mut parts = rest.split_whitespace();
            let old = parts.next().unwrap_or("-0");
            let new = parts.next().unwrap_or("+0");
            let (old_start, _) = parse_range(&old[1..]);
            let (new_start, new_count) = parse_range(&new[1..]);
            hunks.push(RawHunk { old_start, new_start, new_count, del: Vec::new() });
        } else if let (Some(h), Some(d)) = (hunks.last_mut(), l.strip_prefix('-')) {
            h.del.push(clean(d));
        }
    }
    hunks
}

/// Expand tabs, drop CRs and replace control characters so they can't break the TUI.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.trim_end_matches('\r').chars() {
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push('·'),
            c => out.push(c),
        }
    }
    out
}

// ── Syntax highlighting ─────────────────────────────────────────────────────

type Hl = Vec<(Style, String)>;

struct Highlighter {
    ss: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    fn new() -> Self {
        let theme = ThemeSet::load_from_reader(&mut io::Cursor::new(include_str!("tokyonight.tmTheme")))
            .expect("bundled theme is valid");
        Self { ss: two_face::syntax::extra_newlines(), theme }
    }

    fn syntax_for(&self, path: &Path, first: &str) -> &SyntaxReference {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        self.ss
            .find_syntax_by_extension(name)
            .or_else(|| self.ss.find_syntax_by_extension(ext))
            .or_else(|| self.ss.find_syntax_by_first_line(first))
            .unwrap_or_else(|| self.ss.find_syntax_plain_text())
    }

    fn run(&self, syn: &SyntaxReference, lines: &[String]) -> Vec<Hl> {
        if lines.len() > 50_000 {
            return lines.iter().map(|l| plain(l)).collect();
        }
        let mut h = HighlightLines::new(syn, &self.theme);
        lines
            .iter()
            .map(|l| match h.highlight_line(&format!("{l}\n"), &self.ss) {
                Ok(parts) => parts
                    .into_iter()
                    .filter_map(|(st, t)| {
                        let t = t.trim_end_matches('\n');
                        (!t.is_empty()).then(|| (to_style(st), t.to_string()))
                    })
                    .collect(),
                Err(_) => plain(l),
            })
            .collect()
    }
}

fn plain(l: &str) -> Hl {
    vec![(Style::new().fg(FG), l.to_string())]
}

fn to_style(s: syntect::highlighting::Style) -> Style {
    let c = s.foreground;
    let mut st = Style::new().fg(Color::Rgb(c.r, c.g, c.b));
    if s.font_style.contains(FontStyle::ITALIC) {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::BOLD) {
        st = st.add_modifier(Modifier::BOLD);
    }
    st
}

// ── File view (one "buffer") ────────────────────────────────────────────────

struct Hunk {
    old_start: usize,
    new_start: usize,
    new_count: usize,
    del: Vec<Hl>,
}

impl Hunk {
    /// New-file line number the deleted lines are shown above.
    fn anchor(&self) -> usize {
        if self.new_count == 0 { self.new_start + 1 } else { self.new_start }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Src {
    /// 0-based index into the file's lines.
    Line(usize),
    /// Deleted line: (hunk index, index within hunk).
    Del(usize, usize),
    /// Folded region: (first 1-based line, count).
    Fold(usize, usize),
}

#[derive(Clone, Copy)]
struct Row {
    src: Src,
    added: bool,
}

impl Row {
    fn is_change(&self) -> bool {
        self.added || matches!(self.src, Src::Del(..))
    }
}

struct FileView {
    rel: String,
    lang: String,
    lines: Vec<Hl>,
    hunks: Vec<Hunk>,
    added: usize,
    removed: usize,
    opened: Vec<bool>,
    rows: Vec<Row>,
    change_rows: Vec<usize>,
    cursor: usize,
    scroll: usize,
    hscroll: usize,
    note: Option<String>,
}

impl FileView {
    fn load(root: &Path, rel: &str, base: &str, hl: &Highlighter) -> Self {
        let abs = root.join(rel);
        let raw = std::fs::read(&abs).unwrap_or_default();
        let mut note = None;
        let text: Vec<String> = if raw.contains(&0) {
            note = Some("binary file — nothing to show".to_string());
            Vec::new()
        } else {
            String::from_utf8_lossy(&raw).lines().map(clean).collect()
        };
        if !abs.exists() {
            note = Some("file deleted from working tree".to_string());
        }

        let syn = hl.syntax_for(&abs, text.first().map(String::as_str).unwrap_or(""));
        let lines = hl.run(syn, &text);

        let tracked = git(root, &["ls-files", "--error-unmatch", "--", rel]).is_ok()
            || git(root, &["cat-file", "-e", &format!("{base}:{rel}")]).is_ok();
        let raw_hunks = if note.is_some() && abs.exists() {
            Vec::new()
        } else if !tracked {
            if !text.is_empty() {
                note.get_or_insert_with(|| "untracked file".to_string());
            }
            vec![RawHunk { old_start: 0, new_start: 1, new_count: text.len(), del: Vec::new() }]
        } else {
            match git(root, &["diff", "--no-color", "--no-ext-diff", "-U0", base, "--", rel]) {
                Ok(out) => parse_diff(&out),
                Err(e) => {
                    note = Some(format!("git diff failed: {e}"));
                    Vec::new()
                }
            }
        };

        let hunks: Vec<Hunk> = raw_hunks
            .into_iter()
            .filter(|h| h.new_count > 0 || !h.del.is_empty())
            .map(|h| Hunk {
                old_start: h.old_start,
                new_start: h.new_start,
                new_count: h.new_count,
                del: hl.run(syn, &h.del),
            })
            .collect();

        Self {
            rel: rel.to_string(),
            lang: syn.name.clone(),
            added: hunks.iter().map(|h| h.new_count).sum(),
            removed: hunks.iter().map(|h| h.del.len()).sum(),
            opened: vec![false; lines.len() + 2],
            lines,
            hunks,
            rows: Vec::new(),
            change_rows: Vec::new(),
            cursor: 0,
            scroll: 0,
            hscroll: 0,
            note,
        }
    }

    fn row_line(&self, r: &Row) -> usize {
        match r.src {
            Src::Line(i) => i + 1,
            Src::Fold(s, _) => s,
            Src::Del(h, _) => self.hunks[h].anchor(),
        }
    }

    /// Recompute visible rows: top `edge` lines, bottom `edge` lines, changes
    /// (+ `ctx` context lines) and manually opened folds.
    fn rebuild(&mut self, edge: usize, ctx: usize) {
        let keep = self.rows.get(self.cursor).map(|r| self.row_line(r));
        let n = self.lines.len();
        let mut vis = self.opened.clone();
        let mut add = vec![false; n + 2];
        for v in vis.iter_mut().take(edge.min(n) + 1).skip(1) {
            *v = true;
        }
        for v in vis.iter_mut().take(n + 1).skip(n.saturating_sub(edge) + 1) {
            *v = true;
        }
        let mut anchors: Vec<Vec<usize>> = vec![Vec::new(); n + 2];
        for (hi, h) in self.hunks.iter().enumerate() {
            let end = (h.new_start + h.new_count).min(n + 1);
            for l in h.new_start.max(1)..end {
                add[l] = true;
                vis[l] = true;
            }
            if !h.del.is_empty() {
                anchors[h.anchor().min(n + 1)].push(hi);
            }
            let (lo, hi_) = if h.new_count == 0 {
                (h.new_start + 1, h.new_start)
            } else {
                (h.new_start, h.new_start + h.new_count - 1)
            };
            for l in lo.saturating_sub(ctx).max(1)..=(hi_ + ctx).min(n) {
                vis[l] = true;
            }
        }

        let mut rows = Vec::new();
        let mut fold_start = 0;
        let mut fold = 0;
        let flush = |rows: &mut Vec<Row>, fold: &mut usize, start: usize| {
            match *fold {
                0 => {}
                // hiding a single line behind a fold row saves nothing
                1 => rows.push(Row { src: Src::Line(start - 1), added: false }),
                n => rows.push(Row { src: Src::Fold(start, n), added: false }),
            }
            *fold = 0;
        };
        for l in 1..=n + 1 {
            if !anchors[l].is_empty() {
                flush(&mut rows, &mut fold, fold_start);
                for &hi in &anchors[l] {
                    for j in 0..self.hunks[hi].del.len() {
                        rows.push(Row { src: Src::Del(hi, j), added: false });
                    }
                }
            }
            if l > n {
                break;
            }
            if vis[l] {
                flush(&mut rows, &mut fold, fold_start);
                rows.push(Row { src: Src::Line(l - 1), added: add[l] });
            } else {
                if fold == 0 {
                    fold_start = l;
                }
                fold += 1;
            }
        }
        flush(&mut rows, &mut fold, fold_start);

        self.change_rows = (0..rows.len())
            .filter(|&i| rows[i].is_change() && (i == 0 || !rows[i - 1].is_change()))
            .collect();
        self.rows = rows;
        self.cursor = match keep {
            Some(line) => {
                let found = self.rows.iter().position(|r| self.row_line(r) >= line);
                found.unwrap_or(self.rows.len().saturating_sub(1))
            }
            None => self.change_rows.first().copied().unwrap_or(0),
        };
    }

    fn move_by(&mut self, d: isize) {
        let max = self.rows.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + d).clamp(0, max) as usize;
    }

    fn jump(&mut self, row: usize, h: usize) {
        self.cursor = row;
        self.scroll = row.saturating_sub(h / 3);
    }

    fn fix_scroll(&mut self, h: usize) {
        if h == 0 {
            return;
        }
        let so = 4.min(h.saturating_sub(1) / 2);
        if self.cursor < self.scroll + so {
            self.scroll = self.cursor.saturating_sub(so);
        }
        if self.cursor + so >= self.scroll + h {
            self.scroll = self.cursor + so + 1 - h;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(h));
    }

    fn change_index(&self) -> usize {
        self.change_rows.iter().filter(|&&r| r <= self.cursor).count()
    }

    fn cur_line(&self) -> usize {
        self.rows.get(self.cursor).map(|r| self.row_line(r)).unwrap_or(0)
    }

    fn open_fold(&mut self, edge: usize, ctx: usize) -> bool {
        if let Some(Row { src: Src::Fold(s, c), .. }) = self.rows.get(self.cursor).copied() {
            for v in &mut self.opened[s..s + c] {
                *v = true;
            }
            self.rebuild(edge, ctx);
            return true;
        }
        false
    }

    fn num_width(&self) -> usize {
        let max_old = self.hunks.iter().map(|h| h.old_start + h.del.len()).max().unwrap_or(0);
        self.lines.len().max(max_old).to_string().len().max(3)
    }
}

// ── Explorer tree ───────────────────────────────────────────────────────────

struct Node {
    depth: usize,
    name: String,
    file: Option<(String, char)>,
}

fn cmp_paths(a: &str, b: &str) -> Ordering {
    let ac: Vec<&str> = a.split('/').collect();
    let bc: Vec<&str> = b.split('/').collect();
    for i in 0..ac.len().min(bc.len()) {
        if ac[i] != bc[i] {
            let (a_dir, b_dir) = (i + 1 < ac.len(), i + 1 < bc.len());
            return b_dir
                .cmp(&a_dir)
                .then_with(|| ac[i].to_lowercase().cmp(&bc[i].to_lowercase()));
        }
    }
    ac.len().cmp(&bc.len())
}

fn build_tree(files: &[(String, char)]) -> Vec<Node> {
    let mut files = files.to_vec();
    files.sort_by(|a, b| cmp_paths(&a.0, &b.0));
    files.dedup_by(|a, b| a.0 == b.0);
    let mut out = Vec::new();
    let mut prev: Vec<String> = Vec::new();
    for (path, st) in &files {
        let comps: Vec<&str> = path.split('/').collect();
        let dirs = &comps[..comps.len() - 1];
        let common = prev.iter().zip(dirs).take_while(|(a, b)| a == *b).count();
        for (d, name) in dirs.iter().enumerate().skip(common) {
            out.push(Node { depth: d, name: name.to_string(), file: None });
        }
        out.push(Node {
            depth: dirs.len(),
            name: comps[comps.len() - 1].to_string(),
            file: Some((path.clone(), *st)),
        });
        prev = dirs.iter().map(|s| s.to_string()).collect();
    }
    out
}

fn file_icon(name: &str) -> (&'static str, Color) {
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match (lower.as_str(), ext) {
        ("cargo.toml" | "cargo.lock", _) => ("\u{e7a8}", ORANGE),
        ("dockerfile", _) => ("\u{f308}", BLUE),
        ("makefile", _) => ("\u{e779}", DARK3),
        (_, "rs") => ("\u{e7a8}", ORANGE),
        (_, "py") => ("\u{e73c}", YELLOW),
        (_, "js" | "mjs" | "cjs") => ("\u{e74e}", YELLOW),
        (_, "ts" | "mts" | "cts") => ("\u{e628}", BLUE),
        (_, "tsx" | "jsx") => ("\u{e7ba}", CYAN),
        (_, "go") => ("\u{e627}", CYAN),
        (_, "lua") => ("\u{e620}", BLUE),
        (_, "md" | "markdown") => ("\u{e73e}", FG),
        (_, "json" | "jsonc") => ("\u{e60b}", YELLOW),
        (_, "toml" | "ini" | "conf" | "cfg") => ("\u{e615}", DARK3),
        (_, "yml" | "yaml") => ("\u{e615}", MAGENTA),
        (_, "sh" | "bash" | "zsh" | "fish") => ("\u{e795}", GREEN),
        (_, "html" | "htm") => ("\u{e736}", ORANGE),
        (_, "css" | "scss" | "sass") => ("\u{e749}", BLUE),
        (_, "c" | "h") => ("\u{e61e}", BLUE),
        (_, "cpp" | "cc" | "hpp" | "cxx") => ("\u{e61d}", BLUE),
        (_, "java" | "kt") => ("\u{e738}", RED),
        (_, "rb") => ("\u{e739}", RED),
        (_, "nix") => ("\u{f313}", BLUE),
        (_, "lock") => ("\u{f023}", DARK3),
        (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp") => ("\u{f1c5}", MAGENTA),
        (_, "gitignore" | "gitattributes") => ("\u{e702}", ORANGE),
        (_, "txt") => ("\u{f15c}", FG_DARK),
        _ => ("\u{f15b}", FG_DARK),
    }
}

fn status_color(st: char) -> Color {
    match st {
        'A' | '?' => GREEN,
        'D' => RED,
        'R' | 'C' => MAGENTA,
        _ => YELLOW,
    }
}

// ── App ─────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Tree,
    Editor,
}

#[derive(PartialEq, Clone, Copy)]
enum Popup {
    None,
    Help,
    WhichKey,
}

struct App {
    cfg: Cfg,
    root: PathBuf,
    base: String,
    repo_name: String,
    branch: String,
    hl: Highlighter,
    files: Vec<(String, char)>,
    tree: Vec<Node>,
    tree_sel: usize,
    tree_scroll: usize,
    show_tree: bool,
    bufs: Vec<FileView>,
    cur: usize,
    focus: Focus,
    popup: Popup,
    pending: Option<char>,
    msg: String,
    quit: bool,
    tree_rect: Rect,
    editor_rect: Rect,
    bufline_rect: Rect,
    /// Screen columns covered by each buffer tab, indexed like `bufs`.
    tab_cols: Vec<std::ops::Range<u16>>,
}

impl App {
    fn new(cfg: Cfg) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let (root, rel) = match &cfg.path {
            Some(p) => {
                let abs = cwd.join(p);
                if abs.is_dir() {
                    bail!("{p} is a directory — pass a file");
                }
                let name = abs.file_name().with_context(|| format!("invalid path: {p}"))?;
                let parent = abs.parent().unwrap_or(&cwd);
                let parent = parent
                    .canonicalize()
                    .with_context(|| format!("directory does not exist: {}", parent.display()))?;
                let root = PathBuf::from(
                    git(&parent, &["rev-parse", "--show-toplevel"])
                        .context("not inside a git repository")?
                        .trim(),
                )
                .canonicalize()?;
                let full = parent.join(name);
                let rel = full
                    .strip_prefix(&root)
                    .context("file is outside the repository")?
                    .to_string_lossy()
                    .replace('\\', "/");
                (root, Some(rel))
            }
            None => {
                let root = git(&cwd, &["rev-parse", "--show-toplevel"])
                    .context("not inside a git repository")?;
                (PathBuf::from(root.trim()), None)
            }
        };
        let base = resolve_base(&root, &cfg.base);
        let repo_name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut app = App {
            branch: branch(&root),
            files: Vec::new(),
            tree: Vec::new(),
            hl: Highlighter::new(),
            base,
            repo_name,
            root,
            cfg,
            tree_sel: 0,
            tree_scroll: 0,
            show_tree: true,
            bufs: Vec::new(),
            cur: 0,
            focus: Focus::Editor,
            popup: Popup::None,
            pending: None,
            msg: String::new(),
            quit: false,
            tree_rect: Rect::default(),
            editor_rect: Rect::default(),
            bufline_rect: Rect::default(),
            tab_cols: Vec::new(),
        };
        app.reload_files(rel.as_deref());
        match rel {
            Some(rel) => app.open(&rel),
            None => {
                if let Some(first) = app.tree.iter().find_map(|n| n.file.clone()) {
                    app.open(&first.0);
                } else {
                    app.focus = Focus::Tree;
                }
            }
        }
        Ok(app)
    }

    fn reload_files(&mut self, extra: Option<&str>) {
        self.files = changed_files(&self.root, &self.base);
        if let Some(rel) = extra {
            if !self.files.iter().any(|f| f.0 == rel) {
                self.files.push((rel.to_string(), ' '));
            }
        }
        for b in &self.bufs {
            if !self.files.iter().any(|f| f.0 == b.rel) {
                self.files.push((b.rel.clone(), ' '));
            }
        }
        self.tree = build_tree(&self.files);
        self.tree_sel = self.tree_sel.min(self.tree.len().saturating_sub(1));
    }

    fn open(&mut self, rel: &str) {
        if let Some(i) = self.bufs.iter().position(|b| b.rel == rel) {
            self.cur = i;
        } else {
            let mut fv = FileView::load(&self.root, rel, &self.base, &self.hl);
            fv.rebuild(self.cfg.edge, self.cfg.ctx);
            self.bufs.push(fv);
            self.cur = self.bufs.len() - 1;
        }
        self.sync_tree();
        self.focus = Focus::Editor;
        let b = &self.bufs[self.cur];
        self.msg = match &b.note {
            Some(n) => n.clone(),
            None if b.hunks.is_empty() => "no changes against base".into(),
            None => format!(
                "\"{}\" {}L, {} change{}",
                b.rel,
                b.lines.len(),
                b.change_rows.len(),
                if b.change_rows.len() == 1 { "" } else { "s" }
            ),
        };
    }

    fn sync_tree(&mut self) {
        if let Some(b) = self.bufs.get(self.cur) {
            if let Some(i) = self.tree.iter().position(|n| n.file.as_ref().is_some_and(|f| f.0 == b.rel)) {
                self.tree_sel = i;
            }
        }
    }

    fn refresh(&mut self) {
        self.branch = branch(&self.root);
        let rels: Vec<String> = self.bufs.iter().map(|b| b.rel.clone()).collect();
        for (i, rel) in rels.iter().enumerate() {
            let (cursor, scroll) = (self.bufs[i].cursor, self.bufs[i].scroll);
            let mut fv = FileView::load(&self.root, rel, &self.base, &self.hl);
            fv.rebuild(self.cfg.edge, self.cfg.ctx);
            fv.cursor = cursor.min(fv.rows.len().saturating_sub(1));
            fv.scroll = scroll;
            self.bufs[i] = fv;
        }
        self.reload_files(None);
        self.sync_tree();
        self.msg = "refreshed".into();
    }

    fn rebuild_all(&mut self) {
        for b in &mut self.bufs {
            b.rebuild(self.cfg.edge, self.cfg.ctx);
        }
    }

    fn switch_buf(&mut self, d: isize) {
        if self.bufs.is_empty() {
            return;
        }
        let n = self.bufs.len() as isize;
        self.cur = ((self.cur as isize + d).rem_euclid(n)) as usize;
        self.sync_tree();
    }

    fn close_buf(&mut self) {
        self.close_buf_at(self.cur);
    }

    fn close_buf_at(&mut self, i: usize) {
        if i >= self.bufs.len() {
            return;
        }
        self.bufs.remove(i);
        if i < self.cur {
            self.cur -= 1;
        }
        self.cur = self.cur.min(self.bufs.len().saturating_sub(1));
        if self.bufs.is_empty() {
            self.focus = Focus::Tree;
        }
        self.sync_tree();
    }

    fn editor_h(&self) -> usize {
        self.editor_rect.height as usize
    }

    fn goto_change(&mut self, forward: bool) {
        let h = self.editor_h();
        let Some(b) = self.bufs.get_mut(self.cur) else { return };
        if b.change_rows.is_empty() {
            self.msg = "no changes".into();
            return;
        }
        let target = if forward {
            b.change_rows.iter().find(|&&r| r > b.cursor).copied()
        } else {
            b.change_rows.iter().rev().find(|&&r| r < b.cursor).copied()
        };
        let (row, wrapped) = match target {
            Some(r) => (r, false),
            None if forward => (b.change_rows[0], true),
            None => (*b.change_rows.last().unwrap(), true),
        };
        b.jump(row, h);
        let total = b.change_rows.len();
        self.msg = format!(
            "change {}/{}{}",
            b.change_index(),
            total,
            if wrapped { "  (wrapped)" } else { "" }
        );
    }

    // ── input ──

    fn on_key(&mut self, k: KeyEvent) {
        use KeyCode::*;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.popup == Popup::Help {
            self.popup = Popup::None;
            return;
        }
        if let Some(p) = self.pending.take() {
            self.popup = Popup::None;
            match (p, k.code) {
                (' ', Char('e')) => {
                    self.show_tree = !self.show_tree;
                    self.focus = if self.show_tree { Focus::Tree } else { Focus::Editor };
                }
                (' ', Char('q')) => self.quit = true,
                (' ', Char('r')) => self.refresh(),
                (' ', Char('?')) => self.popup = Popup::Help,
                (' ', Char('x')) | (' ', Char('d')) => self.close_buf(),
                ('g', Char('g')) => self.top(),
                (']', Char('h' | 'c')) => self.goto_change(true),
                ('[', Char('h' | 'c')) => self.goto_change(false),
                (']', Char('b')) => self.switch_buf(1),
                ('[', Char('b')) => self.switch_buf(-1),
                ('z', Char('R')) => {
                    for b in &mut self.bufs {
                        b.opened.iter_mut().for_each(|v| *v = true);
                    }
                    self.rebuild_all();
                    self.msg = "all folds opened".into();
                }
                ('z', Char('M')) => {
                    for b in &mut self.bufs {
                        b.opened.iter_mut().for_each(|v| *v = false);
                    }
                    self.rebuild_all();
                    self.msg = "all folds closed".into();
                }
                ('z', Char('o' | 'a')) => {
                    self.open_fold();
                }
                _ => {}
            }
            return;
        }
        match k.code {
            Char('c') if ctrl => self.quit = true,
            Char('w') if ctrl => self.close_buf(),
            Char('h') if ctrl => {
                if self.show_tree {
                    self.focus = Focus::Tree
                }
            }
            Char('l') if ctrl => {
                if !self.bufs.is_empty() {
                    self.focus = Focus::Editor
                }
            }
            Char(c @ (' ' | 'g' | ']' | '[' | 'z')) if !ctrl => {
                self.pending = Some(c);
                if c == ' ' {
                    self.popup = Popup::WhichKey;
                }
            }
            Char('q') => self.quit = true,
            Char('?') => self.popup = Popup::Help,
            Esc => self.msg.clear(),
            Tab => {
                self.focus = match self.focus {
                    Focus::Tree if !self.bufs.is_empty() => Focus::Editor,
                    _ if self.show_tree => Focus::Tree,
                    f => f,
                }
            }
            Char('H') => self.switch_buf(-1),
            Char('L') => self.switch_buf(1),
            Char('R') => self.refresh(),
            Char('+' | '=') => {
                self.cfg.ctx += 1;
                self.rebuild_all();
                self.msg = format!("context: {} line(s)", self.cfg.ctx);
            }
            Char('-') => {
                self.cfg.ctx = self.cfg.ctx.saturating_sub(1);
                self.rebuild_all();
                self.msg = format!("context: {} line(s)", self.cfg.ctx);
            }
            _ => match self.focus {
                Focus::Tree => self.tree_key(k),
                Focus::Editor => self.editor_key(k),
            },
        }
    }

    fn top(&mut self) {
        match self.focus {
            Focus::Tree => self.tree_sel = 0,
            Focus::Editor => {
                if let Some(b) = self.bufs.get_mut(self.cur) {
                    b.cursor = 0;
                }
            }
        }
    }

    fn open_fold(&mut self) -> bool {
        let (edge, ctx) = (self.cfg.edge, self.cfg.ctx);
        let opened = self.bufs.get_mut(self.cur).is_some_and(|b| b.open_fold(edge, ctx));
        if opened {
            self.msg = "fold opened".into();
        }
        opened
    }

    fn open_all_folds(&mut self) {
        let (edge, ctx) = (self.cfg.edge, self.cfg.ctx);
        if let Some(b) = self.bufs.get_mut(self.cur) {
            b.opened.iter_mut().for_each(|v| *v = true);
            b.rebuild(edge, ctx);
            self.msg = "all folds in this file opened".into();
        }
    }

    fn tree_key(&mut self, k: KeyEvent) {
        use KeyCode::*;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.tree.len();
        match k.code {
            Char('j') if ctrl => self.tree_sel = (self.tree_sel + 10).min(n.saturating_sub(1)),
            Char('k') if ctrl => self.tree_sel = self.tree_sel.saturating_sub(10),
            Char('j') | Down => self.tree_sel = (self.tree_sel + 1).min(n.saturating_sub(1)),
            Char('k') | Up => self.tree_sel = self.tree_sel.saturating_sub(1),
            Char('G') | End => self.tree_sel = n.saturating_sub(1),
            Home => self.tree_sel = 0,
            Enter | Char('l' | 'o') | Right => {
                if let Some((rel, _)) = self.tree.get(self.tree_sel).and_then(|n| n.file.clone()) {
                    self.open(&rel);
                }
            }
            Char('r') => self.refresh(),
            _ => {}
        }
    }

    fn editor_key(&mut self, k: KeyEvent) {
        use KeyCode::*;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let h = self.editor_h().max(1) as isize;
        if matches!(k.code, Enter) {
            if ctrl {
                self.open_all_folds();
            } else {
                self.open_fold();
            }
            return;
        }
        // like vim's 'foldopen' "hor": moving right on a fold opens it
        if matches!(k.code, Char('l') | Right) && self.open_fold() {
            return;
        }
        match k.code {
            Char('n') => return self.goto_change(true),
            Char('N') => return self.goto_change(false),
            _ => {}
        }
        let Some(b) = self.bufs.get_mut(self.cur) else { return };
        match k.code {
            Char('j') if ctrl => b.move_by(10),
            Char('k') if ctrl => b.move_by(-10),
            Char('j') | Down => b.move_by(1),
            Char('k') | Up => b.move_by(-1),
            Char('d') if ctrl => b.move_by(h / 2),
            Char('u') if ctrl => b.move_by(-h / 2),
            Char('f') if ctrl => b.move_by(h),
            Char('b') if ctrl => b.move_by(-h),
            PageDown => b.move_by(h),
            PageUp => b.move_by(-h),
            Char('G') | End => b.cursor = b.rows.len().saturating_sub(1),
            Home => b.cursor = 0,
            Char('h') | Left => b.hscroll = b.hscroll.saturating_sub(4),
            Char('l') | Right => b.hscroll += 4,
            Char('0') => b.hscroll = 0,
            _ => {}
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let pos = ratatui::layout::Position { x: m.column, y: m.row };
        let in_tree = self.show_tree && self.tree_rect.contains(pos);
        let in_editor = self.editor_rect.contains(pos);
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d = if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                if in_tree {
                    let max = self.tree.len().saturating_sub(1) as isize;
                    self.tree_sel = (self.tree_sel as isize + d).clamp(0, max) as usize;
                } else if let Some(b) = self.bufs.get_mut(self.cur) {
                    b.move_by(d);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if in_tree {
                    let y = (m.row - self.tree_rect.y) as usize;
                    if y >= 1 {
                        let i = self.tree_scroll + y - 1;
                        if i < self.tree.len() {
                            self.tree_sel = i;
                            self.focus = Focus::Tree;
                            if let Some((rel, _)) = self.tree[i].file.clone() {
                                self.open(&rel);
                            }
                        }
                    }
                } else if in_editor {
                    if let Some(b) = self.bufs.get_mut(self.cur) {
                        let i = b.scroll + (m.row - self.editor_rect.y) as usize;
                        self.focus = Focus::Editor;
                        if i < b.rows.len() {
                            b.cursor = i;
                            self.open_fold();
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.bufline_rect.contains(pos) => {
                if let Some(i) = self.tab_cols.iter().position(|r| r.contains(&m.column)) {
                    self.close_buf_at(i);
                }
            }
            _ => {}
        }
    }

    // ── drawing ──

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Block::new().style(Style::new().bg(BG).fg(FG)), area);
        let [top, mid, status, cmd] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let tree_w = if self.show_tree { 32.min(area.width / 3) } else { 0 };
        let [tree_a, ed_a] =
            Layout::horizontal([Constraint::Length(tree_w), Constraint::Fill(1)]).areas(mid);
        self.tree_rect = tree_a;
        self.editor_rect = ed_a;
        self.bufline_rect = top;

        self.draw_bufferline(f, top, tree_w);
        if self.show_tree {
            self.draw_tree(f, tree_a);
        }
        self.draw_editor(f, ed_a);
        self.draw_statusline(f, status);
        self.draw_cmdline(f, cmd);

        match self.popup {
            Popup::Help => self.draw_help(f, area),
            Popup::WhichKey => self.draw_whichkey(f, ed_a),
            Popup::None => {}
        }
    }

    fn draw_bufferline(&mut self, f: &mut Frame, a: Rect, tree_w: u16) {
        let mut spans = Vec::new();
        let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>() as u16;
        self.tab_cols.clear();
        if tree_w > 0 {
            let title = format!("\u{f07c}  Explorer");
            let pad = (tree_w as usize).saturating_sub(title.chars().count()) / 2;
            let right = (tree_w as usize).saturating_sub(pad + title.chars().count());
            spans.push(Span::styled(
                format!("{}{}{}", " ".repeat(pad), title, " ".repeat(right)),
                Style::new().bg(BG_DARK).fg(BLUE).add_modifier(Modifier::BOLD),
            ));
        }
        for (i, b) in self.bufs.iter().enumerate() {
            let x0 = a.x + width(&spans);
            let name = b.rel.rsplit('/').next().unwrap_or(&b.rel);
            let (icon, ic) = file_icon(name);
            let active = i == self.cur;
            let bg = if active { BG } else { BG_DARK };
            let ind = if active { BLUE } else { BG_DARK };
            spans.push(Span::styled("▎", Style::new().bg(bg).fg(ind)));
            spans.push(Span::styled(
                format!(" {icon} "),
                Style::new().bg(bg).fg(if active { ic } else { COMMENT }),
            ));
            let mut ns = Style::new().bg(bg).fg(if active { FG } else { COMMENT });
            if active {
                ns = ns.add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(name.to_string(), ns));
            if b.added + b.removed > 0 {
                spans.push(Span::styled(" ●", Style::new().bg(bg).fg(if active { YELLOW } else { DARK3 })));
            }
            spans.push(Span::styled("  ", Style::new().bg(bg)));
            self.tab_cols.push(x0..(a.x + width(&spans)).min(a.right()));
        }
        f.render_widget(Line::from(spans).style(Style::new().bg(BG_DARK)), a);
    }

    fn draw_tree(&mut self, f: &mut Frame, a: Rect) {
        f.render_widget(Block::new().style(Style::new().bg(BG_DARK)), a);
        let h = a.height.saturating_sub(1) as usize;
        if self.tree_sel < self.tree_scroll {
            self.tree_scroll = self.tree_sel;
        } else if h > 0 && self.tree_sel >= self.tree_scroll + h {
            self.tree_scroll = self.tree_sel + 1 - h;
        }
        let header = Line::from(vec![
            Span::styled(" \u{f07c} ", Style::new().fg(BLUE)),
            Span::styled(self.repo_name.clone(), Style::new().fg(BLUE).add_modifier(Modifier::BOLD)),
        ]);
        f.render_widget(header, Rect { height: 1, ..a });
        let focused = self.focus == Focus::Tree;
        let cur_rel = self.bufs.get(self.cur).map(|b| b.rel.as_str());
        if self.tree.is_empty() {
            let r = Rect { y: a.y + 2, height: 1, ..a };
            f.render_widget(Line::styled("   no changes ✓", Style::new().fg(COMMENT)), r);
        }
        let w = a.width as usize;
        for (k, node) in self.tree.iter().enumerate().skip(self.tree_scroll).take(h) {
            let row = Rect { y: a.y + 1 + (k - self.tree_scroll) as u16, height: 1, ..a };
            let sel = k == self.tree_sel;
            let bg = if sel && focused { BG_HL } else { BG_DARK };
            let mut indent = String::from(" ");
            for _ in 0..node.depth + 1 {
                indent.push_str("│ ");
            }
            let mut spans = vec![Span::styled(indent, Style::new().fg(GUTTER))];
            let status;
            match &node.file {
                None => {
                    spans.push(Span::styled("\u{f07c} ", Style::new().fg(BLUE)));
                    spans.push(Span::styled(node.name.clone(), Style::new().fg(BLUE)));
                    status = String::new();
                }
                Some((rel, st)) => {
                    let (icon, ic) = file_icon(&node.name);
                    spans.push(Span::styled(format!("{icon} "), Style::new().fg(ic)));
                    let mut ns = Style::new().fg(if *st == ' ' { FG_DARK } else { status_color(*st) });
                    if Some(rel.as_str()) == cur_rel {
                        ns = ns.add_modifier(Modifier::BOLD);
                    }
                    if *st == 'D' {
                        ns = ns.add_modifier(Modifier::CROSSED_OUT);
                    }
                    spans.push(Span::styled(node.name.clone(), ns));
                    status = match st {
                        '?' => "U".into(),
                        ' ' => String::new(),
                        c => c.to_string(),
                    };
                }
            }
            let used: usize = spans.iter().map(|s| s.width()).sum();
            if !status.is_empty() {
                let pad = w.saturating_sub(used + status.len() + 1);
                spans.push(Span::raw(" ".repeat(pad)));
                let st = node.file.as_ref().map(|f| f.1).unwrap_or(' ');
                spans.push(Span::styled(status, Style::new().fg(status_color(st))));
            }
            f.render_widget(Line::from(spans).style(Style::new().bg(bg)), row);
        }
    }

    fn draw_editor(&mut self, f: &mut Frame, a: Rect) {
        let focused = self.focus == Focus::Editor;
        let h = a.height as usize;
        let Some(b) = self.bufs.get_mut(self.cur) else {
            return draw_dashboard(f, a);
        };
        b.fix_scroll(h);
        let nw = b.num_width();
        let text_w = (a.width as usize).saturating_sub(nw + 4);

        if b.rows.is_empty() {
            let msg = b.note.clone().unwrap_or_else(|| "empty file".into());
            let r = Rect { y: a.y + a.height / 2, height: 1, ..a };
            f.render_widget(Line::styled(msg, Style::new().fg(COMMENT)).centered(), r);
            return;
        }

        for (ri, row) in b.rows.iter().enumerate().skip(b.scroll).take(h) {
            let is_cur = ri == b.cursor;
            let (bg, sign, sign_fg) = match row.src {
                Src::Del(..) => (if is_cur { DEL_CUR } else { DEL_BG }, "▎", RED),
                Src::Fold(..) => (if is_cur { BG_HL } else { BG_FOLD }, " ", COMMENT),
                Src::Line(_) if row.added => (if is_cur { ADD_CUR } else { ADD_BG }, "▎", GREEN),
                Src::Line(_) => (if is_cur && focused { BG_HL } else { BG }, " ", COMMENT),
            };
            let num = match row.src {
                Src::Line(i) => format!("{:>nw$}", i + 1),
                Src::Del(hi, j) => format!("{:>nw$}", b.hunks[hi].old_start + j),
                Src::Fold(..) => format!("{:>nw$}", "\u{f460}"),
            };
            let num_style = match row.src {
                _ if is_cur => Style::new().fg(ORANGE).add_modifier(Modifier::BOLD),
                Src::Del(..) => Style::new().fg(Color::Rgb(0x91, 0x4c, 0x54)),
                Src::Fold(..) => Style::new().fg(COMMENT),
                _ => Style::new().fg(GUTTER),
            };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(num, num_style),
                Span::raw(" "),
                Span::styled(sign, Style::new().fg(sign_fg)),
                Span::raw(" "),
            ];
            match row.src {
                Src::Line(i) => spans.extend(slice_spans(&b.lines[i], b.hscroll, text_w)),
                Src::Del(hi, j) => spans.extend(slice_spans(&b.hunks[hi].del[j], b.hscroll, text_w)),
                Src::Fold(s, c) => {
                    let label = format!(
                        "··· {c} lines hidden ({s}–{}) ···",
                        s + c - 1
                    );
                    spans.push(Span::styled(label, Style::new().fg(COMMENT).add_modifier(Modifier::ITALIC)));
                }
            }
            let r = Rect { y: a.y + (ri - b.scroll) as u16, height: 1, ..a };
            f.render_widget(Line::from(spans).style(Style::new().bg(bg)), r);
        }
    }

    fn draw_statusline(&self, f: &mut Frame, a: Rect) {
        let (mode, mode_c) = match self.focus {
            Focus::Editor => (" NORMAL ", BLUE),
            Focus::Tree => (" EXPLORER ", MAGENTA),
        };
        let sec_b = Style::new().bg(GUTTER).fg(mode_c);
        let mut left = vec![
            Span::styled(mode, Style::new().bg(mode_c).fg(BG_DARK).add_modifier(Modifier::BOLD)),
            Span::styled("\u{e0b0}", Style::new().bg(GUTTER).fg(mode_c)),
            Span::styled(format!(" \u{e725} {} ", self.branch), sec_b),
            Span::styled("\u{e0b0}", Style::new().bg(BG_DARK).fg(GUTTER)),
        ];
        let mut right = Vec::new();
        if let Some(b) = self.bufs.get(self.cur) {
            let name = b.rel.rsplit('/').next().unwrap_or(&b.rel);
            let (icon, ic) = file_icon(name);
            left.push(Span::styled(format!(" {icon} "), Style::new().fg(ic)));
            left.push(Span::styled(b.rel.clone(), Style::new().fg(FG)));
            left.push(Span::styled(format!("  +{}", b.added), Style::new().fg(GREEN)));
            left.push(Span::styled(format!(" -{}", b.removed), Style::new().fg(RED)));

            let total = b.change_rows.len();
            if total > 0 {
                right.push(Span::styled(
                    format!("\u{f06c2} {}/{}  ", b.change_index(), total),
                    Style::new().fg(MAGENTA),
                ));
            }
            right.push(Span::styled(
                format!("ctx {}  ", self.cfg.ctx),
                Style::new().fg(COMMENT),
            ));
            right.push(Span::styled(format!("{}  ", b.lang), Style::new().fg(FG_DARK)));
            let pct = match (b.scroll, b.rows.len()) {
                (0, _) => "Top".to_string(),
                (s, n) if s + self.editor_h() >= n => "Bot".to_string(),
                (_, n) => format!("{}%", b.cursor * 100 / n.max(1)),
            };
            right.push(Span::styled("\u{e0b2}", Style::new().bg(BG_DARK).fg(GUTTER)));
            right.push(Span::styled(format!(" {pct} "), sec_b));
            right.push(Span::styled("\u{e0b2}", Style::new().bg(GUTTER).fg(mode_c)));
            right.push(Span::styled(
                format!(" {}:{} ", b.cur_line(), b.hscroll + 1),
                Style::new().bg(mode_c).fg(BG_DARK).add_modifier(Modifier::BOLD),
            ));
        } else {
            right.push(Span::styled(format!(" {} files changed ", self.files.len()), sec_b));
        }
        let lw: usize = left.iter().map(|s| s.width()).sum();
        let rw: usize = right.iter().map(|s| s.width()).sum();
        left.push(Span::raw(" ".repeat((a.width as usize).saturating_sub(lw + rw))));
        left.extend(right);
        f.render_widget(Line::from(left).style(Style::new().bg(BG_DARK)), a);
    }

    fn draw_cmdline(&self, f: &mut Frame, a: Rect) {
        let msg = if self.msg.is_empty() {
            Span::styled(
                "]h/[h next/prev change · <Enter>/l/click open fold · +/- context · <Space>e explorer · ? help",
                Style::new().fg(DARK3),
            )
        } else {
            Span::styled(self.msg.clone(), Style::new().fg(FG))
        };
        f.render_widget(Line::from(msg), a);
        if let Some(p) = self.pending {
            let s = if p == ' ' { "<Space>".to_string() } else { p.to_string() };
            let w = s.len() as u16 + 2;
            let r = Rect { x: a.x + a.width.saturating_sub(w), width: w.min(a.width), ..a };
            f.render_widget(Line::styled(s, Style::new().fg(FG_DARK)), r);
        }
    }

    fn draw_whichkey(&self, f: &mut Frame, a: Rect) {
        let items = [
            ("e", "Explorer toggle"),
            ("r", "Refresh git state"),
            ("x", "Close buffer"),
            ("?", "Keymaps"),
            ("q", "Quit"),
        ];
        let per_row = ((a.width as usize).saturating_sub(4) / 26).max(1);
        let h = ((items.len().div_ceil(per_row)) as u16 + 3).min(a.height);
        let r = Rect { y: a.y + a.height - h, height: h, ..a };
        f.render_widget(Clear, r);
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(BLUE))
            .title(Span::styled(" <Space> ", Style::new().fg(ORANGE).add_modifier(Modifier::BOLD)))
            .style(Style::new().bg(BG_DARK));
        let inner = block.inner(r);
        f.render_widget(block, r);
        let lines: Vec<Line> = items
            .chunks(per_row)
            .map(|chunk| {
                let mut spans = vec![Span::raw("  ")];
                for (k, d) in chunk {
                    spans.push(Span::styled(*k, Style::new().fg(CYAN).add_modifier(Modifier::BOLD)));
                    spans.push(Span::styled(" ➜ ", Style::new().fg(COMMENT)));
                    spans.push(Span::styled(format!("{d:<22}"), Style::new().fg(MAGENTA)));
                }
                Line::from(spans)
            })
            .collect();
        f.render_widget(Paragraph::new(lines), Rect { y: inner.y + (inner.height > 1) as u16, ..inner });
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let keys: &[(&str, &str)] = &[
            ("j / k", "move down / up"),
            ("C-j / C-k", "move 10 lines down / up"),
            ("C-d / C-u", "half page down / up"),
            ("gg / G", "top / bottom"),
            ("]h / [h   n / N", "next / previous change"),
            ("h / l / 0", "scroll left / right / reset"),
            ("Enter / l / zo", "open fold under cursor"),
            ("mouse click", "open fold / move cursor"),
            ("C-Enter", "open all folds in this file"),
            ("zR / zM", "open / close all folds"),
            ("+ / -", "more / less context around changes"),
            ("H / L   [b / ]b", "previous / next buffer"),
            ("Tab  C-h / C-l", "switch explorer / editor"),
            ("<Space>e", "toggle explorer"),
            ("<Space>x / C-w", "close buffer"),
            ("right-click tab", "close that buffer"),
            ("R", "refresh git state"),
            ("q", "quit"),
        ];
        let w = 64.min(area.width);
        let h = (keys.len() as u16 + 4).min(area.height);
        let r = Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + (area.height - h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, r);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(BLUE))
            .title(Span::styled(" 󰌌 Keymaps ", Style::new().fg(ORANGE).add_modifier(Modifier::BOLD)))
            .title_bottom(Line::styled(" any key to close ", Style::new().fg(COMMENT)).centered())
            .style(Style::new().bg(BG_DARK));
        let lines: Vec<Line> = std::iter::once(Line::raw(""))
            .chain(keys.iter().map(|(k, d)| {
                Line::from(vec![
                    Span::styled(format!("  {k:>17}"), Style::new().fg(CYAN)),
                    Span::styled("  ➜  ", Style::new().fg(COMMENT)),
                    Span::styled(*d, Style::new().fg(FG)),
                ])
            }))
            .collect();
        f.render_widget(Paragraph::new(lines).block(block), r);
    }
}

fn draw_dashboard(f: &mut Frame, a: Rect) {
    let mut lines: Vec<Line> = LOGO
        .iter()
        .map(|l| Line::styled(*l, Style::new().fg(BLUE).add_modifier(Modifier::BOLD)).centered())
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled("working tree is clean — nothing to show", Style::new().fg(COMMENT)).centered());
    lines.push(Line::raw(""));
    for (k, d) in [("R", "Refresh"), ("?", "Keymaps"), ("q", "Quit")] {
        lines.push(
            Line::from(vec![
                Span::styled(format!("{d:<24}"), Style::new().fg(FG)),
                Span::styled(k, Style::new().fg(ORANGE)),
            ])
            .centered(),
        );
    }
    let top = a.height.saturating_sub(lines.len() as u16) / 2;
    f.render_widget(Paragraph::new(lines), Rect { y: a.y + top, height: a.height - top, ..a });
}

/// Cut a highlighted line to the visible window [skip, skip+take).
fn slice_spans(hl: &Hl, skip: usize, take: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut pos = 0;
    for (st, text) in hl {
        let len = text.chars().count();
        let (start, end) = (pos, pos + len);
        pos = end;
        if end <= skip {
            continue;
        }
        if start >= skip + take {
            break;
        }
        let from = skip.saturating_sub(start);
        let to = (skip + take - start).min(len);
        out.push(Span::styled(text.chars().skip(from).take(to - from).collect::<String>(), *st));
    }
    out
}

// ── Plain print mode ────────────────────────────────────────────────────────

fn ansi_fg(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m"),
        _ => String::new(),
    }
}

fn ansi_bg(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("\x1b[48;2;{r};{g};{b}m"),
        _ => String::new(),
    }
}

fn print_view(out: &mut impl Write, b: &FileView, color: bool) -> io::Result<()> {
    let nw = b.num_width();
    let rs = if color { "\x1b[0m" } else { "" };
    let c = |col: Color| if color { ansi_fg(col) } else { String::new() };
    writeln!(
        out,
        "{}── {} {}+{}{} {}-{}{} ──{rs}",
        c(BLUE),
        b.rel,
        c(GREEN),
        b.added,
        c(BLUE),
        c(RED),
        b.removed,
        c(BLUE)
    )?;
    if let Some(n) = &b.note {
        writeln!(out, "{}   {n}{rs}", c(COMMENT))?;
    }
    for row in &b.rows {
        let (bg, sign, sfg) = match row.src {
            Src::Del(..) => (Some(DEL_BG), "-", RED),
            Src::Line(_) if row.added => (Some(ADD_BG), "+", GREEN),
            _ => (None, " ", COMMENT),
        };
        let bgs = match (color, bg) {
            (true, Some(bg)) => ansi_bg(bg),
            _ => String::new(),
        };
        let (num, hl) = match row.src {
            Src::Line(i) => (format!("{:>nw$}", i + 1), Some(&b.lines[i])),
            Src::Del(hi, j) => (format!("{:>nw$}", b.hunks[hi].old_start + j), Some(&b.hunks[hi].del[j])),
            Src::Fold(s, n) => {
                writeln!(out, "{}{:>nw$}   ··· {n} lines hidden ({s}–{}) ···{rs}", c(COMMENT), "", s + n - 1)?;
                continue;
            }
        };
        write!(out, "{bgs}{}{num} {}{sign} ", c(GUTTER), c(sfg))?;
        for (st, t) in hl.into_iter().flatten() {
            write!(out, "{}{t}", st.fg.map(c).unwrap_or_default())?;
        }
        writeln!(out, "{}{rs}", if color && bg.is_some() { "\x1b[K" } else { "" })?;
    }
    Ok(())
}

// ── main ────────────────────────────────────────────────────────────────────

fn main() {
    if let Err(e) = real_main() {
        eprintln!("gitdif: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cfg = parse_args()?;
    let stdout_tty = io::stdout().is_terminal();
    if cfg.print || !stdout_tty {
        let color = stdout_tty && std::env::var_os("NO_COLOR").is_none();
        let app = App::new(cfg)?;
        let mut out = io::stdout().lock();
        let mut views: Vec<&FileView> = app.bufs.iter().collect();
        let extra: Vec<FileView>;
        if app.cfg.path.is_none() {
            extra = app
                .tree
                .iter()
                .filter_map(|n| n.file.as_ref())
                .filter(|(rel, _)| !app.bufs.iter().any(|b| &b.rel == rel))
                .map(|(rel, _)| {
                    let mut fv = FileView::load(&app.root, rel, &app.base, &app.hl);
                    fv.rebuild(app.cfg.edge, app.cfg.ctx);
                    fv
                })
                .collect();
            views.extend(extra.iter());
        }
        for (i, v) in views.iter().enumerate() {
            if i > 0 {
                writeln!(out)?;
            }
            print_view(&mut out, v, color)?;
        }
        return Ok(());
    }

    let mut app = App::new(cfg)?;
    let mut term = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    // Kitty keyboard protocol, so keys like Ctrl+Enter are distinguishable
    // from their plain versions.
    let kitty_keys = supports_keyboard_enhancement().unwrap_or(false);
    if kitty_keys {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let res = run(&mut app, &mut term);
    if kitty_keys {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

fn run(app: &mut App, term: &mut DefaultTerminal) -> Result<()> {
    while !app.quit {
        term.draw(|f| app.draw(f))?;
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
            Event::Mouse(m) => app.on_mouse(m),
            _ => {}
        }
    }
    Ok(())
}
