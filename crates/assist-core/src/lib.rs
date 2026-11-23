//! Text-only core of assist (PLAN.md section 3).
//!
//! Config, phrases, matcher, dialog, policy, jobs and the audit log. Nothing here touches
//! the mic, speaker, screen or network; its only I/O is its own data directory.

pub mod action;
pub mod audit;
pub mod config;
pub mod dialog;
pub mod matcher;
pub mod normalize;
pub mod phrase;
pub mod phrases;
pub mod policy;
mod private_fs;
pub mod scheduler;
