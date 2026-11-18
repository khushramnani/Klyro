# Klyro

One binary that lets any AI agent see and use anything with a screen: websites, Electron apps, documents.

Status: early MVP.

License: Apache-2.0.

## Engines

By default each session starts on the fast lane and moves to the visual lane on its own when a
step needs it (a screenshot, or something the fast lane cannot do). `klyro status` shows the
lane and every switch with its reason.

- `lightpanda` (fast lane): no pixels, much lighter. Linux and macOS run it natively; Windows
  runs it inside WSL. To let it reach servers on Windows' localhost, set
  `networkingMode=mirrored` under `[wsl2]` in `%USERPROFILE%\.wslconfig`, then `wsl --shutdown`.
- `chromium` (visual lane): Chrome, Chromium or Edge. Real layout and screenshots. Used from
  the start when Lightpanda is not installed, or with `--headed`.

Pin one lane for a session, which turns automatic switching off:
`klyro open --engine chromium <url>` (or `KLYRO_ENGINE`).

## Desktop apps (Windows)

`klyro app "Calculator"` attaches to an open window by title; `klyro app --launch <program> -- [args]`
starts one. The same commands then work on its controls through Windows UI Automation.
macOS and Linux apps are not supported yet.

## Documents

`klyro open report.pdf` (or `.md`, `.html`) shows a local file on the visual lane. `text`
gives PDF text page by page; `screenshot --page 2` captures one PDF page. Office files
(docx, pptx, xlsx) are not supported yet.

## Use from an agent

- **Skill (Claude Code, Codex, any agent with a shell):** `skill/SKILL.md` teaches the commands.
- **MCP:** `klyro mcp` serves the same commands as MCP tools over stdio, with the same compact
  text; screenshots also come back as images.
  - Claude Code: `claude mcp add klyro -- klyro mcp`
  - Other clients: `{ "mcpServers": { "klyro": { "command": "klyro", "args": ["mcp"] } } }`

## Third-party code

- [pdf.js](https://github.com/mozilla/pdf.js) 6.5.438 (Apache-2.0), unmodified build files in
  `crates/adapter-web/vendor/pdfjs`, embedded to render PDFs.
