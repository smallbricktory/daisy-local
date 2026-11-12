// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Local MCP (Model Context Protocol) server — loopback-only, read-only
//! access to the meeting library for MCP clients on this machine
//! (Claude Code, Claude Desktop, …). See docs/MCP.md.

pub mod protocol;
pub mod server;
pub mod token;
pub mod tools;
