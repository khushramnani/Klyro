# Klyro

One binary that lets any AI agent see and use anything with a screen: websites, Electron apps, documents.

Status: early MVP.

License: Apache-2.0.

## Engines

- `chromium` (default for now): Chrome, Chromium or Edge. Real layout and screenshots.
- `lightpanda`: fast lane, no pixels. Linux and macOS run it natively; Windows runs it inside
  WSL. To let it reach servers on Windows' localhost, set `networkingMode=mirrored` under
  `[wsl2]` in `%USERPROFILE%\.wslconfig`, then `wsl --shutdown`.

Pick one per session: `klyro open --engine lightpanda <url>` (or `KLYRO_ENGINE`).
