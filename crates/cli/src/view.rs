//! `klyro view`: the session's screen live in this terminal.
//!
//! Frames are polled from the daemon, one at a time with a gap after each, so a viewer never
//! crowds the agent's commands; after a slow frame the gap grows. ponytail: polling; switch to
//! Page.startScreencast if a viewer measurably slows agents.

use crate::render::{self, Image};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use klyro_daemon::client::send;
use klyro_daemon::ipc::Request;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Render {
    Blocks,
    Sixel,
}

pub struct Opts {
    pub session: String,
    pub render: Option<Render>,
    pub once: bool,
    /// Terminal size when stdout is not a terminal (or to override it).
    pub size: Option<(u16, u16)>,
}

/// Sixel where the terminal is known to have it, else half-blocks (work everywhere).
fn detect() -> Render {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let sixel_term = std::env::var_os("WT_SESSION").is_some() || env("TERM_PROGRAM") == "WezTerm" || env("TERM").starts_with("foot") || env("TERM").contains("mlterm");
    if sixel_term && std::io::stdout().is_terminal() { Render::Sixel } else { Render::Blocks }
}

/// Pixels per cell, for sizing sixel images. ponytail: 8x16 when the terminal won't say (Windows);
/// too small only shrinks the picture, too large would scroll the screen.
fn cell_px(cols: u16, rows: u16) -> (usize, usize) {
    match terminal::window_size() {
        Ok(s) if s.width > 0 && s.height > 0 => ((s.width / cols.max(1)) as usize, (s.height / rows.max(1)) as usize),
        _ => (8, 16),
    }
}

enum Frame {
    Pixels(Image),
    Text(String, String),
}

fn request(session: &str, cmd: &str, args: Vec<String>) -> Result<serde_json::Value, String> {
    let req = Request { token: String::new(), session: session.into(), cmd: cmd.into(), args, json: true, headed: false, engine: None };
    let r = send(req, false)?;
    if !r.ok {
        return Err(r.out);
    }
    serde_json::from_str(&r.out).map_err(|e| format!("bad reply: {e}"))
}

/// Returns the frame and the raw payload (to skip redraws when nothing changed).
fn fetch(session: &str, max_w: usize) -> Result<(Frame, String), String> {
    let v = request(session, "view-frame", vec![max_w.to_string()])?;
    if let Some(png) = v["png"].as_str() {
        let bytes = klyro_adapter_web::base64_decode(png).ok_or("frame was not base64")?;
        return Ok((Frame::Pixels(Image::decode(&bytes)?), png.to_string()));
    }
    let (text, status) = (v["text"].as_str().unwrap_or("").to_string(), v["status"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string());
    Ok((Frame::Text(text.clone(), status), text))
}

/// The screen as terminal output for an area of cols x rows cells (top-left at 1;1).
fn draw(frame: &Frame, how: Render, cols: u16, rows: u16) -> String {
    let (cols, rows) = (cols as usize, rows as usize);
    match frame {
        Frame::Pixels(img) => match how {
            Render::Blocks => {
                let (w, h) = render::fit(img.w, img.h, cols, rows * 2);
                render::blocks(&img.scale(w, h)).iter().enumerate().map(|(i, r)| format!("\x1b[{};1H{r}\x1b[K", i + 1)).collect()
            }
            Render::Sixel => {
                let (cw, ch) = cell_px(cols as u16, rows as u16);
                let (w, h) = render::fit(img.w, img.h, cols * cw, rows * ch);
                format!("\x1b[1;1H{}", render::sixel(&img.scale(w, h)))
            }
        },
        Frame::Text(text, _) => text
            .lines()
            .take(rows)
            .enumerate()
            .map(|(i, l)| format!("\x1b[{};1H{}\x1b[K", i + 1, l.chars().take(cols).collect::<String>()))
            .collect(),
    }
}

fn status_line(o: &Opts, frame: &Frame, how: Render, cols: u16) -> String {
    let what = match frame {
        Frame::Pixels(_) => format!("{how:?}").to_lowercase(),
        Frame::Text(_, lane) => format!("{lane}: no pixels, showing text; v moves to Chromium (reloads the page)"),
    };
    let quit = if o.once { "" } else { " · q quits" };
    let s = format!(" klyro view · session {} · {what}{quit} ", o.session);
    format!("\x1b[7m{}\x1b[0m", s.chars().take(cols as usize).collect::<String>())
}

/// Puts the terminal back however we leave (return, error or panic).
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(std::io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
    }
}

pub fn run(o: Opts) -> ExitCode {
    match run_inner(&o) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(o: &Opts) -> Result<(), String> {
    let how = o.render.unwrap_or_else(detect);
    let tty = std::io::stdout().is_terminal();
    let size = || o.size.or_else(|| tty.then(|| terminal::size().ok()).flatten()).unwrap_or((100, 30));
    let max_w = |cols: u16, rows: u16| match how {
        Render::Blocks => cols as usize,
        Render::Sixel => cols as usize * cell_px(cols, rows).0,
    };
    let mut out = std::io::stdout();

    if o.once {
        let (cols, rows) = size();
        let (frame, _) = fetch(&o.session, max_w(cols, rows))?;
        let area = rows.saturating_sub(1).max(1);
        // Rows are positioned for a full screen; print them in order instead.
        let body = strip_positions(&draw(&frame, how, cols, area).replace("\x1b[K", "\x1b[K\n"));
        writeln!(out, "{body}{}", status_line(o, &frame, how, cols)).map_err(|e| e.to_string())?;
        return Ok(());
    }

    if !tty {
        return Err("klyro view needs a terminal; use --once to print one frame".into());
    }
    terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    let _restore = Restore;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        drop(Restore);
        hook(p);
    }));
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide, terminal::Clear(terminal::ClearType::All)).map_err(|e| e.to_string())?;

    let mut last = String::new();
    loop {
        let (cols, rows) = size();
        let started = Instant::now();
        match fetch(&o.session, max_w(cols, rows)) {
            Ok((frame, raw)) if raw != last => {
                if matches!(frame, Frame::Text(..)) || how == Render::Blocks {
                    // Shorter content must not leave old lines behind.
                    write!(out, "\x1b[2J").ok();
                }
                let area = rows.saturating_sub(1).max(1);
                write!(out, "{}\x1b[{rows};1H{}", draw(&frame, how, cols, area), status_line(o, &frame, how, cols)).ok();
                out.flush().ok();
                last = raw;
            }
            Ok(_) => {}
            Err(e) if e.starts_with("no session") => return Err(e),
            Err(e) => {
                write!(out, "\x1b[{rows};1H\x1b[7m frame failed: {} \x1b[0m\x1b[K", e.chars().take(cols as usize - 16).collect::<String>()).ok();
                out.flush().ok();
            }
        }
        // A slow frame means a busy browser: give the agent room.
        let gap = if started.elapsed() > Duration::from_secs(1) { Duration::from_secs(2) } else { Duration::from_millis(100) };
        let until = Instant::now() + gap;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if !event::poll(left).map_err(|e| e.to_string())? {
                break;
            }
            match event::read().map_err(|e| e.to_string())? {
                Event::Key(k) if k.code == KeyCode::Char('q') || (k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c' | 'q'))) => return Ok(()),
                Event::Key(k) if k.code == KeyCode::Char('v') => {
                    let _ = request(&o.session, "view-input", vec!["visual".into()]);
                    last.clear();
                }
                Event::Resize(..) => {
                    write!(out, "\x1b[2J").ok();
                    last.clear();
                }
                _ => {}
            }
        }
    }
}

/// `\x1b[<row>;1H` cursor moves -> nothing (for printing rows in sequence).
fn strip_positions(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("\x1b[") {
        out += &rest[..i];
        let tail = &rest[i + 2..];
        let end = tail.find(|c: char| !(c.is_ascii_digit() || c == ';')).unwrap_or(tail.len());
        if tail[end..].starts_with('H') {
            rest = &tail[end + 1..];
        } else {
            out += "\x1b[";
            rest = tail;
        }
    }
    out + rest
}

#[cfg(test)]
mod tests {
    #[test]
    fn strip_positions_keeps_colours() {
        assert_eq!(super::strip_positions("\x1b[1;1H\x1b[38;2;1;2;3mA\x1b[2;1HB"), "\x1b[38;2;1;2;3mAB");
    }
}
