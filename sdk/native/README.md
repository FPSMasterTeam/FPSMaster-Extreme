# Native mods

A native mod is a Rust `cdylib` built against the **`fpsmaster_ext_api`** crate. It
gets the full hook surface (the same JSON command/query/HUD protocol as JS) plus
unrestricted native code — own threads/state, heavy CPU work, and a render-geometry
hook. The host loads it after an `abi_stable` layout + version check (a mismatch is
rejected, not crashed).

The `fpsmaster_ext_api` crate is **bundled in this SDK** (`fpsmaster_ext_api/`), so no
crates.io is needed — the template/example depend on it via a relative path
(`{ path = "../fpsmaster_ext_api" }`). Alternatives, if you'd rather not carry the
folder around: a git dependency
(`fpsmaster_ext_api = { git = "https://github.com/FPSMasterTeam/FPSMaster-Extreme" }`), or
crates.io if it ever gets published. (`abi_stable` itself does come from crates.io
— that's a normal third-party dependency.)

## Build

`template/` is a minimal starter, `example/` is a complete working mod. Both
path-depend on the bundled `fpsmaster_ext_api/`, so build from inside the SDK.

```sh
cd template          # or example
cargo build --release
# → target/release/lib<name>.dylib   (macOS)
#                  lib<name>.so       (Linux)
#                  <name>.dll         (Windows)
```

## Install

Drop the built library and a `mod.toml` into a fpsmaster `mods/<id>/` folder:

```
mods/my_native_mod/
  mod.toml
  libmy_native_mod.dylib        # the built cdylib (per OS × arch)
```

`mod.toml` (note `entry` is the actual library filename):

```toml
id = "my.native.mod"
version = "0.1.0"
tier = "native"
api = "^0.3"
entry = "libmy_native_mod.dylib"   # .so on Linux, .dll on Windows
```

Native mods are **not** cross-platform — ship one binary per OS × arch.

## Notes

- `abi_stable`'s macros expand to `::abi_stable` paths, so depend on it directly
  at the same version `fpsmaster_ext_api` uses (currently `0.11`).
- Rebuild your cdylib whenever `fpsmaster_ext_api` changes; an old binary is
  rejected at load by the layout/version check.
- Obfuscation: a stripped release host still loads native mods — they depend only
  on `fpsmaster_ext_api`'s checked layout, never on host symbols, and the cdylib
  keeps its root-module export in the dynamic symbol table even when stripped.

See `../REFERENCE.md` (§ Native mods) for the full hook surface and the JSON
protocol.
