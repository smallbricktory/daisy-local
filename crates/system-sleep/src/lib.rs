// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! System sleep, per OS: an inhibitor that keeps the machine from idling
//! into sleep, and a monitor that reports suspend and resume.

pub mod inhibit;
pub mod monitor;

pub use inhibit::SleepInhibitor;
pub use monitor::{PowerEvent, PowerMonitor};
