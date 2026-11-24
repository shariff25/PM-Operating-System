//! Test-only crate: `tests/stack.rs` runs the real bot runtime, tool server and `pmos-web`
//! routers together in one process (memory stores, fake connectors, user assertions in
//! `require` with real keys), the way CI can without building images. Nothing here ships.
