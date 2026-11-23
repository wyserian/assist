//! Text-only core of assist (PLAN.md §3).
//!
//! Phase 0: config loading, phrase syntax, normalization and the rule matcher.
//! Nothing here touches the mic, speaker, screen, OS or network.

pub mod config;
pub mod dialog;
pub mod matcher;
pub mod normalize;
pub mod phrase;
pub mod phrases;
pub mod policy;
