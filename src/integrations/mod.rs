//! Adapters between outside services and the canonical [`model`](crate::model).
//!
//! Each adapter's job is translation: read a service's own format and hand back
//! model types that follow the model's rules, or take model types and write
//! them out. Recommendation logic never lives here, and a service's wire
//! format never leaks past its module.

pub mod spotify;
