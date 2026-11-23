//! Shared message and data contracts between server roles.
//!
//! Callers depend on these instead of on the peer service crate (BW-16-01-02).
//! Wire layouts mirror the reference implementation:
//! `programming/bigworld/lib/network/machine_guard.hpp`.

pub mod machined;
