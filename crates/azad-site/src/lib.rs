//! The site of Azad's orchestrator (decision 0034), in Leptos: pages of the orchestrator's runs, their steps and their
//! output. The server renders each page from the runs' records (`ssr`); the browser gets the same pages as WebAssembly
//! (`hydrate`), which keeps a page of a running run up to date.

// Leptos's views are nested types, as deep as the pages are; the compiler's default limit is too low for them.
#![recursion_limit = "256"]

pub mod ansi;
pub mod app;
pub mod model;
#[cfg(feature = "ssr")]
pub mod server;

/// The events of a stream of a running step's output.
pub const LINE_EVENT: &str = "line";
pub const END_EVENT: &str = "end";

// wasm-bindgen's export of the entry point is generated unsafe code, which the workspace otherwise denies.
#[cfg(feature = "hydrate")]
#[allow(unsafe_code)]
pub mod hydration {
    #[wasm_bindgen::prelude::wasm_bindgen]
    pub fn hydrate() {
        console_error_panic_hook::set_once();
        leptos::mount::hydrate_body(crate::app::App);
    }
}
