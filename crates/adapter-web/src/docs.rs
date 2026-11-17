//! Local documents (PDF, Markdown, HTML), shown through the visual lane.
//!
//! A small server on 127.0.0.1 hands each opened document to Chromium under a random token:
//! browsers block ES modules and relative assets on file://. PDFs render with pdf.js
//! (vendored, Apache-2.0); Markdown is turned into HTML.

use crate::cdp::engine_err;
use klyro_core::{Error, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const PDFJS: &[u8] = include_bytes!("../vendor/pdfjs/pdf.min.mjs");
const PDFJS_WORKER: &[u8] = include_bytes!("../vendor/pdfjs/pdf.worker.min.mjs");
const PDFJS_RENDERER: &[u8] = include_bytes!("../vendor/pdfjs/pdf.renderer.min.mjs");

/// Renders every page to a canvas, adds each page's text for the accessibility tree, and
/// exposes `window.klyro = { pages, text: [..], ready, error }`.
const PDF_VIEWER: &str = r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<title>TITLE</title>
<style>
  body { margin: 0; background: #e5e5e5; }
  /* Left-aligned: a full-page capture drops the scrollbar, which would shift centred pages. */
  section { width: fit-content; margin: 0 0 12px; background: white; }
  canvas { display: block; }
  .text { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); }
</style>
<script type="module">
  import * as pdfjs from './pdf.mjs';
  window.klyro = { pages: 0, text: [], ready: false, error: null };
  try {
    pdfjs.GlobalWorkerOptions.workerSrc = './pdf.worker.mjs';
    pdfjs.GlobalWorkerOptions.rendererSrc = './pdf.renderer.mjs';
    const doc = await pdfjs.getDocument({ url: './doc.pdf' }).promise;
    window.klyro.pages = doc.numPages;
    for (let i = 1; i <= doc.numPages; i++) {
      const page = await doc.getPage(i);
      const vp = page.getViewport({ scale: 1.5 });
      const sec = document.createElement('section');
      sec.id = 'page-' + i;
      sec.setAttribute('aria-label', 'Page ' + i);
      const canvas = document.createElement('canvas');
      canvas.width = Math.floor(vp.width);
      canvas.height = Math.floor(vp.height);
      canvas.setAttribute('role', 'presentation');
      sec.append(canvas);
      document.body.append(sec);
      await page.render({ canvas, canvasContext: canvas.getContext('2d'), viewport: vp }).promise;
      const tc = await page.getTextContent();
      const text = tc.items.map(t => t.str + (t.hasEOL ? '\n' : '')).join('').trim();
      window.klyro.text.push(text);
      const h = document.createElement('h2');
      h.className = 'text';
      h.textContent = 'Page ' + i;
      const p = document.createElement('div');
      p.className = 'text';
      p.textContent = text;
      sec.prepend(h, p);
    }
  } catch (e) {
    window.klyro.error = String(e && e.message || e);
  }
  window.klyro.ready = true;
</script>
</html>
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Pdf,
    Markdown,
    Html,
}

/// `Some(kind)` for a local file Klyro can show, `Err` for a file type it can't yet, `None`
/// when `target` is not a local file (a URL).
pub fn doc_kind(target: &str) -> Result<Option<(DocKind, PathBuf)>> {
    let path = PathBuf::from(target.strip_prefix("file:///").or_else(|| target.strip_prefix("file://")).unwrap_or(target));
    if !path.is_file() {
        return Ok(None);
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let kind = match ext.as_str() {
        "pdf" => DocKind::Pdf,
        "md" | "markdown" => DocKind::Markdown,
        "html" | "htm" => DocKind::Html,
        "docx" | "doc" | "pptx" | "ppt" | "xlsx" | "xls" | "odt" | "odp" | "ods" | "rtf" => {
            return Err(Error::Invalid(format!(
                ".{ext} files are not supported yet; Klyro opens PDF, Markdown and HTML files (export to PDF to check it)"
            )));
        }
        _ => {
            return Err(Error::Invalid(format!(
                "don't know how to open .{ext} files; Klyro opens PDF, Markdown and HTML files"
            )));
        }
    };
    Ok(Some((kind, std::fs::canonicalize(&path).unwrap_or(path))))
}

struct Doc {
    kind: DocKind,
    path: PathBuf,
}

struct Server {
    port: u16,
    docs: Mutex<HashMap<String, Doc>>,
}

fn server() -> Result<&'static Server> {
    static SERVER: OnceLock<Server> = OnceLock::new();
    if let Some(s) = SERVER.get() {
        return Ok(s);
    }
    let listener = TcpListener::bind("127.0.0.1:0").map_err(engine_err)?;
    let port = listener.local_addr().map_err(engine_err)?.port();
    let s = SERVER.get_or_init(|| Server { port, docs: Mutex::default() });
    if s.port == port {
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = serve(stream);
                });
            }
        });
    }
    Ok(s)
}

/// URL Chromium should open for this document. The token keeps other local programs from
/// browsing the document's folder.
pub fn register(kind: DocKind, path: PathBuf) -> Result<String> {
    let s = server()?;
    let token = random_token();
    // ponytail: registered documents live until the daemon exits; prune if sessions open thousands.
    s.docs.lock().unwrap().insert(token.clone(), Doc { kind, path });
    Ok(format!("http://127.0.0.1:{}/{token}/", s.port))
}

fn random_token() -> String {
    use std::hash::{BuildHasher, RandomState};
    (0..2).map(|i| format!("{:016x}", RandomState::new().hash_one((i, std::time::SystemTime::now())))).collect()
}

fn serve(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(&stream);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let target = first.split_whitespace().nth(1).unwrap_or("/");
    let target = target.split(['?', '#']).next().unwrap_or("");
    let (status, ctype, body) = match respond(target) {
        Some((ctype, body)) => ("200 OK", ctype, body),
        None => ("404 Not Found", "text/plain", b"not found".to_vec()),
    };
    let mut out = &stream;
    write!(out, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    out.write_all(&body)
}

fn respond(target: &str) -> Option<(&'static str, Vec<u8>)> {
    let rest = target.strip_prefix('/')?;
    let (token, rel) = rest.split_once('/').unwrap_or((rest, ""));
    let docs = server().ok()?.docs.lock().unwrap();
    let doc = docs.get(token)?;
    let rel = percent_decode(rel);
    let name = doc.path.file_name()?.to_string_lossy().to_string();
    match (doc.kind, rel.as_str()) {
        (DocKind::Pdf, "") => Some(("text/html; charset=utf-8", PDF_VIEWER.replace("TITLE", &html_escape(&name)).into_bytes())),
        (DocKind::Pdf, "pdf.mjs") => Some(("text/javascript", PDFJS.to_vec())),
        (DocKind::Pdf, "pdf.worker.mjs") => Some(("text/javascript", PDFJS_WORKER.to_vec())),
        (DocKind::Pdf, "pdf.renderer.mjs") => Some(("text/javascript", PDFJS_RENDERER.to_vec())),
        (DocKind::Pdf, "doc.pdf") => Some(("application/pdf", std::fs::read(&doc.path).ok()?)),
        (DocKind::Markdown, "") => Some(("text/html; charset=utf-8", markdown_page(&std::fs::read_to_string(&doc.path).ok()?, &name).into_bytes())),
        (DocKind::Html, "") => Some(("text/html", std::fs::read(&doc.path).ok()?)),
        // Relative assets (images, CSS) from the document's own folder, never above it.
        (_, rel) => {
            let rel = Path::new(rel);
            if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
                return None;
            }
            let file = doc.path.parent()?.join(rel);
            Some((content_type(&file), std::fs::read(file).ok()?))
        }
    }
}

fn markdown_page(md: &str, name: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};
    let mut body = String::new();
    html::push_html(&mut body, Parser::new_ext(md, Options::all()));
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<meta charset=\"utf-8\">\n<title>{}</title>\n<style>body {{ max-width: 46rem; margin: 2rem auto; padding: 0 1rem; font: 16px/1.5 system-ui, sans-serif; }} pre, code {{ background: #f4f4f4; }} img {{ max-width: 100%; }} table {{ border-collapse: collapse; }} td, th {{ border: 1px solid #ccc; padding: 4px 8px; }}</style>\n<body>\n{body}</body>\n</html>\n",
        html_escape(name)
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        if let (b'%', Some(v)) = (b[i], hex) {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "html" | "htm" => "text/html",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_not_yet() {
        let dir = std::env::temp_dir().join(format!("klyro-doc-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["a.pdf", "b.MD", "c.htm", "d.docx", "e.xyz"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let p = |f: &str| dir.join(f).display().to_string();
        assert_eq!(doc_kind(&p("a.pdf")).unwrap().unwrap().0, DocKind::Pdf);
        assert_eq!(doc_kind(&p("b.MD")).unwrap().unwrap().0, DocKind::Markdown);
        assert_eq!(doc_kind(&p("c.htm")).unwrap().unwrap().0, DocKind::Html);
        assert!(doc_kind(&p("d.docx")).unwrap_err().to_string().contains(".docx files are not supported yet"));
        assert!(doc_kind(&p("e.xyz")).is_err());
        assert!(doc_kind("https://example.com").unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decode_and_render() {
        assert_eq!(percent_decode("my%20file.png"), "my file.png");
        assert_eq!(percent_decode("100%"), "100%");
        let page = markdown_page("# Title\n\n- a\n- b", "x.md");
        assert!(page.contains("<h1>Title</h1>") && page.contains("<li>a</li>"));
    }

    #[test]
    fn assets_never_leave_the_document_folder() {
        let dir = std::env::temp_dir().join(format!("klyro-doc-serve-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("img")).unwrap();
        std::fs::write(dir.join("doc.md"), "# Hi").unwrap();
        std::fs::write(dir.join("img/a.png"), b"png").unwrap();
        let url = register(DocKind::Markdown, dir.join("doc.md")).unwrap();
        let base = url.trim_start_matches("http://").split_once('/').unwrap().1; // "<token>/"
        assert!(respond(&format!("/{base}")).unwrap().1.starts_with(b"<!doctype html>"));
        assert_eq!(respond(&format!("/{base}img/a.png")).unwrap(), ("image/png", b"png".to_vec()));
        assert!(respond(&format!("/{base}../doc.md")).is_none());
        assert!(respond(&format!("/{base}%2e%2e/doc.md")).is_none());
        assert!(respond("/wrongtoken/").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
