# fpsmaster Extension SDK

The extension SDK now lives in **[`sdk/`](../sdk/)** at the repo root — a
self-contained, publishable bundle:

- **[`sdk/README.md`](../sdk/README.md)** — overview + 30-second quickstart
- **[`sdk/REFERENCE.md`](../sdk/REFERENCE.md)** — the full API reference
- `sdk/js/` — TypeScript typings (`fpsmaster.d.ts`) + the JS example mods
- `sdk/native/` — native (`cdylib`) build guide, a template, and a worked example

The runnable example mods this repo loads at startup are in `mods/`; the native
API crate is `crates/fpsmaster_ext_api`, bundled with the SDK as a local snapshot
in `sdk/native/fpsmaster_ext_api/` (no crates.io needed).
