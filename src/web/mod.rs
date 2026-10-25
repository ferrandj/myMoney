//! The web interface: the same pages, forms and behaviour as the Python version (FastAPI + Jinja2).

pub mod app;
pub mod charts;
pub mod cloudflare;
pub mod forms;
pub mod http;
pub mod jinja;
pub mod pwa;
pub mod security;
pub mod serve;
pub mod themes;
pub mod views;
#[cfg(test)]
mod cf_tests;
