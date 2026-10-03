// ABOUTME: MCP module aggregating protocol types, server, tools, and transports
// ABOUTME: Provides the core Model Context Protocol infrastructure generic over state type S
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

pub mod auth;
pub(crate) mod cancellation;
#[cfg(feature = "computation")]
pub mod computation;
pub mod elicitation;
pub mod host;
pub mod logging;
pub mod modern;
pub mod observe;
pub mod pagination;
pub mod protocol;
pub(crate) mod random_id;
pub mod resource_metadata;
pub mod schema;
pub mod server;
pub mod tasks;
pub mod tool;
pub mod transport;
#[cfg(feature = "schema-validation")]
pub mod validation;
