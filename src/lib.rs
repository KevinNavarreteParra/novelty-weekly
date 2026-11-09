//! Novelty Weekly: a weekly playlist where novelty is the objective function.
//!
//! Dependencies point one way, from the edges in to the contract:
//!
//! ```text
//! cli ─► commands ─┬─► integrations ─┐
//!                  ├─► store ────────┼─► model   (the canonical data contract)
//!                  └─► engine ───────┘
//! ```
//!
//! - [`model`]: the canonical types the app runs on. Nothing in it knows about
//!   Spotify, Last.fm or SQLite.
//! - [`integrations`]: adapters that turn an outside service into `model` types.
//! - [`store`]: the SQLite database that holds the canonical state.
//! - [`engine`]: the recommendation logic. Pure functions over `model` types:
//!   no network, no database, so it is tested with plain values.
//! - [`commands`]: one function per CLI command, wiring the other three.

pub mod cli;
pub mod commands;
pub mod config;
pub mod engine;
pub mod integrations;
pub mod model;
pub mod store;
