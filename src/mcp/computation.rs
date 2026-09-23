// ABOUTME: Computation — a tool that parses typed arguments, computes, and renders the result
// ABOUTME: Blanket McpTool impl; the input schema is generated from the type the tool parses
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Tools that are one library operation over their arguments.
//!
//! Most satellite tools take a JSON document, turn it into a library type, call
//! one function and render what it returns. Written by hand, each does three
//! things that drift: an input schema typed out next to a struct that parses
//! something else, a parse error with its own wording, and a render through
//! `serde_json::to_value`/`json!`, which widens every `f32` to its nearest `f64`
//! (12.8 reaches the reader as 12.800000190734863).
//!
//! A [`Computation`] states the operation once — input type, output type, the
//! function — and the blanket [`McpTool`] impl does the rest the same way for
//! every server: the input schema is generated from [`Computation::Input`] by
//! schemars, so it cannot disagree with what is parsed; malformed arguments are
//! a tool error naming the tool; the output is rendered with
//! `serde_json::to_string` of the value itself, so every number is written at
//! its own precision.
//!
//! ```rust,ignore
//! struct RankSources;
//!
//! impl Computation for RankSources {
//!     type Input = RankSourcesInput;
//!     type Output = Ranking;
//!     const NAME: &'static str = "rank_sources";
//!     const TITLE: &'static str = "Rank data sources";
//!     const DESCRIPTION: &'static str = "Rank providers by how much their data is trusted …";
//!
//!     fn compute(&self, input: RankSourcesInput) -> Result<Ranking, String> {
//!         Ok(rank(input))
//!     }
//! }
//!
//! registry.register(Box::new(RankSources));
//! ```
//!
//! The annotations describe a computation: read-only, idempotent, closed-world.
//! A tool that writes, or reaches the network, is not a `Computation` — it
//! implements [`McpTool`] directly and declares its own.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::{schema_for, JsonSchema};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use super::schema::{Tool, ToolAnnotations, ToolResponse};
use super::tool::{McpTool, ToolContext};

/// A tool that is one computation over its arguments.
///
/// Implementing it makes the type an [`McpTool`] for any server state.
pub trait Computation: Send + Sync {
    /// What the arguments parse into. Its derived schema is the tool's input
    /// schema, so a field's doc comment is what the client reads about it.
    type Input: DeserializeOwned + JsonSchema;
    /// What the computation returns, rendered as the tool's text result.
    type Output: Serialize;

    /// Tool name, as `tools/call` addresses it.
    const NAME: &'static str;
    /// Short human-readable title.
    const TITLE: &'static str;
    /// What the tool does, for the model choosing among tools.
    const DESCRIPTION: &'static str;

    /// Run the computation.
    ///
    /// # Errors
    ///
    /// A message for the caller when the input is well-formed but cannot be
    /// computed over — an unknown key, a window that does not fit the range.
    /// It is returned as a tool error prefixed with [`Self::NAME`].
    fn compute(&self, input: Self::Input) -> Result<Self::Output, String>;
}

#[async_trait]
impl<S: Send + Sync + ?Sized, T: Computation> McpTool<S> for T {
    fn definition(&self) -> Tool {
        Tool {
            name: T::NAME.to_owned(),
            description: T::DESCRIPTION.to_owned(),
            input_schema: schema_for!(T::Input).to_value(),
            annotations: Some(ToolAnnotations {
                title: Some(T::TITLE.to_owned()),
                read_only_hint: Some(true),
                destructive_hint: Some(false),
                idempotent_hint: Some(true),
                open_world_hint: Some(false),
            }),
            output_schema: None,
            execution: None,
        }
    }

    async fn execute(&self, _state: &Arc<S>, _ctx: &ToolContext, arguments: Value) -> ToolResponse {
        run(self, arguments)
    }
}

/// Parse, compute, render — or the tool error for whichever step failed.
fn run<T: Computation + ?Sized>(tool: &T, arguments: Value) -> ToolResponse {
    let input = match serde_json::from_value::<T::Input>(arguments) {
        Ok(input) => input,
        Err(e) => return ToolResponse::error(format!("{}: invalid arguments: {e}", T::NAME)),
    };
    let output = match tool.compute(input) {
        Ok(output) => output,
        Err(message) => return ToolResponse::error(format!("{}: {message}", T::NAME)),
    };
    match serde_json::to_string(&output) {
        Ok(text) => ToolResponse::text(text),
        Err(e) => ToolResponse::error(format!("{}: could not render the result: {e}", T::NAME)),
    }
}
