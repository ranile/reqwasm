//! HTTP requests library for WASM apps. It provides idiomatic Rust bindings for the `web_sys`
//! `fetch` and `WebSocket` API.
//!
//! This re-exports _all_ the API from [gloo_net] crate.

pub use gloo_net::*;
