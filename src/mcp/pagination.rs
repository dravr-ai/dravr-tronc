// ABOUTME: Cursor pagination for MCP list methods: an opaque cursor naming the last item served
// ABOUTME: Pages a list sorted by key, so a page boundary survives items added or removed between calls
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Cursor pagination for MCP list methods (`tools/list`, and any
//! `resources/list` or `prompts/list` a host serves).
//!
//! The specification leaves the page size to the server and makes the cursor
//! opaque to the client: it echoes a `nextCursor` back as `params.cursor`, and
//! a response without `nextCursor` is the last page. Here the cursor encodes
//! the key of the last item served, and items are ordered by key, so the next
//! page starts at the first key after it. That keeps a page boundary
//! meaningful when the list changes between calls: an item removed in the
//! meantime cannot invalidate the cursor, the way an offset or an exact-match
//! lookup would.

use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;

/// A cursor this server never issued: not text, not base64url, or not UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCursor;

impl fmt::Display for InvalidCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid cursor")
    }
}

impl Error for InvalidCursor {}

/// One page of a list method's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The items on this page, in key order.
    pub items: Vec<T>,
    /// The cursor for the next page, `None` on the last one.
    pub next_cursor: Option<String>,
}

/// The `cursor` a list request carries in `params`, `None` for the first page.
///
/// # Errors
///
/// [`InvalidCursor`] for a `cursor` that is present and not a string.
pub fn cursor_param(params: Option<&Value>) -> Result<Option<&str>, InvalidCursor> {
    match params.and_then(|p| p.get("cursor")) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(cursor)) => Ok(Some(cursor)),
        Some(_) => Err(InvalidCursor),
    }
}

/// Encode `key` as an opaque cursor.
fn encode_cursor(key: &str) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

/// Decode a cursor back to the key it names.
fn decode_cursor(cursor: &str) -> Result<String, InvalidCursor> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| InvalidCursor)?;
    String::from_utf8(bytes).map_err(|_| InvalidCursor)
}

/// The page of `items` after `cursor`, at most `page_size` long.
///
/// `items` are ordered by `key` first. Without a `page_size` the page runs to
/// the end of the list and carries no `next_cursor`; with one, a page that
/// stops short of the end carries the cursor for the rest.
///
/// # Errors
///
/// [`InvalidCursor`] for a cursor this function never issued.
pub fn paginate<T>(
    mut items: Vec<T>,
    cursor: Option<&str>,
    page_size: Option<NonZeroUsize>,
    key: impl Fn(&T) -> &str,
) -> Result<Page<T>, InvalidCursor> {
    items.sort_by(|a, b| key(a).cmp(key(b)));
    let start = match cursor {
        Some(cursor) => {
            let after = decode_cursor(cursor)?;
            items.partition_point(|item| key(item) <= after.as_str())
        }
        None => 0,
    };
    let mut rest: Vec<T> = items.into_iter().skip(start).collect();
    let next_cursor = match page_size {
        Some(size) if rest.len() > size.get() => {
            rest.truncate(size.get());
            rest.last().map(|last| encode_cursor(key(last)))
        }
        _ => None,
    };
    Ok(Page {
        items: rest,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(page: &Page<String>) -> Vec<&str> {
        page.items.iter().map(String::as_str).collect()
    }

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn unpaged_lists_everything_in_key_order() {
        let page = paginate(list(&["c", "a", "b"]), None, None, String::as_str).expect("page"); // Safe: test assertion
        assert_eq!(names(&page), ["a", "b", "c"]);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn pages_follow_their_cursors_to_the_end() {
        let size = NonZeroUsize::new(2);
        let all = list(&["e", "d", "c", "b", "a"]);
        let first = paginate(all.clone(), None, size, String::as_str).expect("page"); // Safe: test assertion
        assert_eq!(names(&first), ["a", "b"]);
        let second = paginate(
            all.clone(),
            first.next_cursor.as_deref(),
            size,
            String::as_str,
        )
        .expect("page"); // Safe: test assertion
        assert_eq!(names(&second), ["c", "d"]);
        let last =
            paginate(all, second.next_cursor.as_deref(), size, String::as_str).expect("page"); // Safe: test assertion
        assert_eq!(names(&last), ["e"]);
        assert_eq!(last.next_cursor, None, "the last page carries no cursor");
    }

    #[test]
    fn an_exactly_full_last_page_carries_no_cursor() {
        let page = paginate(
            list(&["a", "b"]),
            None,
            NonZeroUsize::new(2),
            String::as_str,
        )
        .expect("page"); // Safe: test assertion
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn a_cursor_survives_its_item_being_removed() {
        let size = NonZeroUsize::new(2);
        let first =
            paginate(list(&["a", "b", "c", "d"]), None, size, String::as_str).expect("page"); // Safe: test assertion
                                                                                              // "b", the item the cursor names, is gone by the next call.
        let next = paginate(
            list(&["a", "c", "d"]),
            first.next_cursor.as_deref(),
            size,
            String::as_str,
        )
        .expect("page"); // Safe: test assertion
        assert_eq!(names(&next), ["c", "d"]);
    }

    #[test]
    fn a_cursor_never_issued_is_refused() {
        for cursor in ["***", "not base64!", "/w"] {
            assert_eq!(
                paginate(list(&["a"]), Some(cursor), None, String::as_str),
                Err(InvalidCursor),
                "{cursor}"
            );
        }
    }

    #[test]
    fn the_cursor_param_must_be_a_string() {
        assert_eq!(cursor_param(None), Ok(None));
        assert_eq!(cursor_param(Some(&json!({}))), Ok(None));
        assert_eq!(
            cursor_param(Some(&json!({ "cursor": "YQ" }))),
            Ok(Some("YQ"))
        );
        assert_eq!(
            cursor_param(Some(&json!({ "cursor": 3 }))),
            Err(InvalidCursor)
        );
    }
}
