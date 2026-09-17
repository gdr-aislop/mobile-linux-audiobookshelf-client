//! Small reusable widgets shared across screens. Most of these are compatibility shims standing
//! in for libadwaita APIs newer than this crate's `v1_2` feature ceiling (PureOS Crimson's actual
//! shipped version — see the architecture plan), each documenting exactly which real widget/
//! version it's covering for so it can be deleted the day the minimum target version moves past
//! it; `cover_image` is the exception — a plain graceful-degradation wrapper, not a version shim.

pub mod banner;
pub mod cover_image;
pub mod item_card;
