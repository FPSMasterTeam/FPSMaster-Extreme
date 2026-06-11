# Verification log

## 2026-06-11

Environment:

- macOS host
- Rust `cargo 1.95.0`, `rustc 1.95.0`
- GPU backend observed by wgpu: Metal / Apple M4
- Local server: Paper 1.8.8 build 445, protocol 47, `online-mode=false`

Commands run successfully:

```bash
cargo test -p recraft_core -p recraft_protocol -p recraft_render
cargo check
```

Local Paper setup:

```bash
python3 scripts/setup_paper_1_8_test_server.py
local_server/paper-1.8-protocol47/run.sh
```

Server startup reached:

```text
Starting minecraft server version 1.8.8
Done (...)! For help, type "help" or "?"
```

Client demo runtime check:

```bash
RUST_LOG=warn cargo run -p recraft_app
```

No wgpu validation error was observed during the short run.

Client offline-mode server connection check:

```bash
RUST_LOG=info cargo run -p recraft_app -- --connect 127.0.0.1:25565 --username ReCraftBot3
```

Observed client logs:

```text
logged in as ReCraftBot3 (...)
applied chunk bulk: 10 chunks
...
applied chunk bulk: 9 chunks
```

Observed server logs:

```text
ReCraftBot3[/127.0.0.1:...] logged in with entity id ... at ([world]..., ..., ...)
```

Known issue found and fixed during this verification:

- Sending serverbound `PlayerPositionLook` (`0x06`) to this PaperSpigot build caused `Bad packet id 6` disconnects.
- Current movement sender uses `PlayerPosition` (`0x04`) as a conservative baseline. Look packet support still needs deeper protocol/server compatibility investigation.

Not yet verified:

- Visual correctness of loaded world beyond runtime logs.
- Vanilla-exact player physics parity.
- Block light / sky light rendering.
- Online-mode authentication/encryption.
