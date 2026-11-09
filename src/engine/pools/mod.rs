//! Candidate pools (spec §7.2). Each turns the profile plus knowledge about
//! artists into [`Candidate`](super::candidate::Candidate)s. A pool only ranks;
//! the hard rules live in [`filters`](super::filters) and are applied to every
//! pool alike.
//!
//! Pools that need data fetched for their own intermediate results (near needs
//! top tracks for the artists it scored) are split into stages, so the caller
//! does the fetching between them and the pools stay free of I/O.

pub mod deep;
pub mod far;
pub mod near;
