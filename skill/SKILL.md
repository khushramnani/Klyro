---
name: klyro
description: See and use websites, Electron apps, native Windows apps and local documents (PDF, Markdown, HTML) from the command line with `klyro`. Use when a task needs a web page, a web app, a desktop app or a document checked, read, filled in, clicked through or screenshotted.
---

# Klyro

`klyro` drives a browser for you. Each command is one shell call; a background daemon keeps
the page open between calls. Work from text first; take a screenshot only when you need to
see pixels.

## The loop

1. `klyro open <url-or-file>`: open a page (`example.com`, `http://localhost:3000`, `report.pdf`).
2. `klyro snapshot -i`: list the interactive elements with refs:
   ```
   - textbox "Email" [ref=e4]
   - button "Sign in" [ref=e5]
   ```
   `klyro snapshot` (no `-i`) shows the whole page as a tree, including text.
3. Act on refs: `klyro fill e4 "me@example.com"`, `klyro click e5`, `klyro press Enter`.
4. Read the output of the action. It lists only what changed:
   - `+ text: Wrong password` (appeared), `- button "Sign in" [ref=e5]` (gone),
     `~ textbox "Email" [ref=e4] [value="me@example.com"]` (changed).
   - `page changed: <url> (title)` followed by the new page's outline (elements with refs,
     headings, alert/status messages), when a new page loaded. `text` gives its content.
   - `no change yet`: the page may update later; use `klyro wait` or `klyro snapshot`.
5. Repeat. Refs stay the same while their element stays on the page.

## Commands

| Command | Does |
|---|---|
| `open <url\|file>` | Open a URL or a local `.pdf`, `.md`, `.html` file |
| `snapshot [-i]` | Page tree with refs (`-i`: interactive elements only) |
| `click <ref>` | Click |
| `fill <ref> <text>` | Replace a text field's content |
| `press <key> [ref]` | `Enter`, `Tab`, `Escape`, `ArrowDown`, `Control+a`, ... |
| `text` | Readable page text (PDF: per page) |
| `wait <ms>` / `wait --text <t>` / `wait --url <part>` | Wait (default timeout 10 s, `--timeout <ms>`) |
| `screenshot [file.png] [--full] [--ref <ref>] [--page <n>]` | Save a PNG; prints path and size |
| `connect <port>` | Attach to a running Electron app started with `--remote-debugging-port=<port>` |
| `app "<window title>"` / `app --launch <program> -- [args]` | Drive a native Windows app (attach to an open window, or start one) |
| `status` | Which engine the session uses, and any switches |
| `record start` / `record stop <file.json>` | Save this session's actions as a replayable recording |
| `replay <file.json>` | Run a recording again (elements found by role and name; `${NAME}` = env var) |
| `expect --text <t>` / `--url <part>` / `--role <r> [--name <n>]` `[--not]` | Test assertion; exits 1 if it doesn't hold. Recorded |
| `close` | Close the session (an attached app keeps running) |
| `doctor` | What this machine supports |

Options for any command: `--session <name>` (separate pages and cookies; default `default`),
`--json` (machine-readable output).

## Errors and what to do

- `stale ref @e5` / `unknown ref @e5`: the page changed. Run `klyro snapshot -i` and use the new refs.
- `no session '...'`: run `klyro open <url>` first.
- `timed out ... waiting for ...`: the condition did not happen; check with `klyro text` or `klyro snapshot`.
- `... is not supported yet`: that file type can't be opened (Office files); ask for a PDF.
- Any failing command exits non-zero with one `error:` line on stderr.

## Good to know

- Klyro picks a fast text-only engine first and moves to a real browser by itself when a
  step needs pixels (a note on stderr says so). The move reloads the page: cookies, the URL
  and typed form values come along, other in-page state (open menus, scroll position, data
  a script kept in memory) does not. If that matters, start with `klyro open --engine chromium <url>`.
- Refs can be written `e5` or `@e5`. Password fields show a masked value.
- Windows apps: `click` uses the control's own action (no mouse) when it can; `press` and
  screenshots bring the app's window to the front. `close` stops an app klyro started and
  leaves an attached one running.
- A human can follow along with `klyro watch` (a live page with the screen and your actions);
  nothing changes for you. `klyro open --headed <url>` shows a real browser window instead.
- When the user wants a flow they can rerun without you (a smoke test, a daily check), run
  `klyro record start` before it and `klyro record stop <file>` after, then check it with
  `klyro replay <file>`. Put secrets in the file as `${NAME}` instead of the real value.
- Close sessions you no longer need: `klyro close` (and `--session <name>` for others).
