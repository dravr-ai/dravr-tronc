// ABOUTME: Server-side JSON Schema validation of a tool call's arguments and its structured result
// ABOUTME: A tool's inputSchema/outputSchema compiled once, checked on every call (feature schema-validation)
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! A tool's declared schemas, enforced.
//!
//! A tool's `inputSchema` tells the model what to send and its `outputSchema`
//! tells the client what comes back, and without validation both are promises
//! nothing checks: a handler reads whatever arguments arrive, and a result
//! that drifted from its schema reaches a client that validates it and
//! discards it.
//!
//! [`ToolSchemaValidator`] compiles both schemas of one [`Tool`] once and
//! checks each call against them. [`ToolRegistry`](crate::mcp::tool::ToolRegistry)
//! compiles one per tool when it is registered and checks every call it
//! executes. A host that installs a
//! [`ToolDispatcher`](crate::mcp::host::ToolDispatcher) owns its whole call,
//! validation included, and compiles a validator per definition the same way.
//!
//! Both checks answer with a tool error (`isError: true`), never a protocol
//! error: revision 2025-11-25 reports input validation as a tool execution
//! error so the model can read what was wrong and correct its call. A schema
//! without `$schema` is read as JSON Schema 2020-12, the dialect the
//! specification makes the default.

use std::error::Error as StdError;
use std::fmt;

use jsonschema::Validator;
use serde_json::Value;

use crate::mcp::schema::{Tool, ToolResponse};

/// Separator between violations in one message.
const VIOLATION_SEPARATOR: &str = "; ";

/// The compiled `inputSchema` and `outputSchema` of one tool.
pub struct ToolSchemaValidator {
    input: Validator,
    output: Option<Validator>,
}

impl fmt::Debug for ToolSchemaValidator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolSchemaValidator")
            .field("output", &self.output.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolSchemaValidator {
    /// Compile `tool`'s schemas.
    ///
    /// # Errors
    ///
    /// A [`SchemaCompileError`] naming the schema that is not a valid JSON
    /// Schema, or that references a document outside itself — remote and
    /// file `$ref`s are never resolved.
    pub fn compile(tool: &Tool) -> Result<Self, SchemaCompileError> {
        let input =
            jsonschema::validator_for(&tool.input_schema).map_err(|e| SchemaCompileError {
                schema: SchemaRole::Input,
                reason: e.to_string(),
            })?;
        let output = tool
            .output_schema
            .as_ref()
            .map(jsonschema::validator_for)
            .transpose()
            .map_err(|e| SchemaCompileError {
                schema: SchemaRole::Output,
                reason: e.to_string(),
            })?;
        Ok(Self { input, output })
    }

    /// Check a call's arguments against the `inputSchema`.
    ///
    /// # Errors
    ///
    /// Every way the arguments violate the schema.
    pub fn check_arguments(&self, arguments: &Value) -> Result<(), SchemaViolations> {
        SchemaViolations::collect(&self.input, arguments)
    }

    /// Check a result against the `outputSchema`.
    ///
    /// An error result is not checked: it reports a failure, and the
    /// specification asks for structured content only from a success. A
    /// success from a tool that declares an `outputSchema` must carry
    /// `structuredContent` that conforms to it; one that declares none is
    /// not checked.
    ///
    /// # Errors
    ///
    /// Every way the structured content violates the schema, or a single
    /// violation when it is missing.
    pub fn check_result(&self, response: &ToolResponse) -> Result<(), SchemaViolations> {
        let Some(output) = &self.output else {
            return Ok(());
        };
        if response.is_error {
            return Ok(());
        }
        let Some(structured) = &response.structured_content else {
            return Err(SchemaViolations(vec![
                "the tool declares an outputSchema but returned no structuredContent".to_owned(),
            ]));
        };
        SchemaViolations::collect(output, structured)
    }
}

/// Which of a tool's schemas a [`SchemaCompileError`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaRole {
    /// The `inputSchema`.
    Input,
    /// The `outputSchema`.
    Output,
}

impl fmt::Display for SchemaRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Input => "inputSchema",
            Self::Output => "outputSchema",
        })
    }
}

/// A tool declares a schema that cannot be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaCompileError {
    /// The schema that failed.
    pub schema: SchemaRole,
    /// What the compiler said.
    pub reason: String,
}

impl fmt::Display for SchemaCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "its {} is not a usable JSON Schema: {}",
            self.schema, self.reason
        )
    }
}

impl StdError for SchemaCompileError {}

/// The ways one value violates a schema, each prefixed with the JSON pointer
/// of the offending location (empty for the root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolations(pub Vec<String>);

impl SchemaViolations {
    /// `Ok` when `instance` conforms to `validator`, every violation otherwise.
    fn collect(validator: &Validator, instance: &Value) -> Result<(), Self> {
        let violations: Vec<String> = validator
            .iter_errors(instance)
            .map(|e| {
                let path = e.instance_path().to_string();
                if path.is_empty() {
                    e.to_string()
                } else {
                    format!("{path}: {e}")
                }
            })
            .collect();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(Self(violations))
        }
    }
}

impl fmt::Display for SchemaViolations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join(VIOLATION_SEPARATOR))
    }
}

impl StdError for SchemaViolations {}
