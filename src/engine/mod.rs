//! The recommendation logic, as pure functions over [`model`](crate::model)
//! types: no network, no database, no clock. Callers pass in what was observed
//! and the current time, so every step here is tested with plain values.
//!
//! A build runs these steps in order:
//! 1. [`profile`]: taste and exposure from every play.
//! 2. `pools`: candidates from each pool (deep, near, far).
//! 3. [`select`]: the playlist, split across pools by the bandit.
//! 4. [`feedback`]: how past recommendations fared, which tunes step 3.

pub mod candidate;
pub mod feedback;
pub mod profile;
pub mod select;
