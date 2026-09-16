//! Compatibility widgets standing in for libadwaita APIs newer than this crate's `v1_2` feature
//! ceiling (PureOS Crimson's actual shipped version — see the architecture plan). Each one
//! documents exactly which real widget/version it's covering for, so the shim can be deleted the
//! day the minimum target version moves past it.

pub mod banner;
