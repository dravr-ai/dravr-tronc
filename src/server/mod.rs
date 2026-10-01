// ABOUTME: Server infrastructure modules for REST API and MCP unified servers
// ABOUTME: Provides auth middleware, request guard, shed response, health check trait, CLI args, and tracing initialization
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

pub mod auth;
pub mod cli;
pub mod health;
pub mod request_guard;
pub mod shed;
pub mod tracing_init;
