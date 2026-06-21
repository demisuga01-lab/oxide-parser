//! Geometric document-analysis algorithms that build on the positioned text
//! chunks (and, for tables, drawn graphics) to recover structure Poppler's CLIs
//! don't expose: layout segmentation / reading order ([`layout`]), and — built
//! on top of it — table detection/extraction.
//!
//! These are **additive** — the default text-extraction path is untouched.

pub mod graphics;
pub mod layout;
pub mod tables;
