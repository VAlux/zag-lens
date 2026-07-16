//! Idempotent user-level installation and configuration management.
//!
//! The installer deliberately separates planning from application. Planning
//! parses each configuration file, detects ownership conflicts, and returns a
//! complete [`InstallPlan`] without changing the filesystem. Applying a plan
//! verifies that its inputs have not changed, creates backups, and uses atomic
//! renames for the resulting files.

mod engine;
mod hooks;
mod model;
mod opencode;
mod paths;
mod zellij;

pub use engine::{Installer, restore_backup};
pub use model::{
    ApplyReport, Component, Conflict, FileChange, InstallError, InstallPlan, Notice, Operation,
    PlanContext, Selection,
};
pub use paths::{InstallPaths, PathEnvironment};
