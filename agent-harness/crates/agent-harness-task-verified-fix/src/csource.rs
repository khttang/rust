//! Just enough C text analysis for the acceptance checks.
//!
//! This is not a C parser. It blanks comments and string/char literals
//! (keeping every byte offset and newline), then works on that text: finding
//! a function definition by name and brace matching, extracting assertion
//! calls, and listing preprocessor lines. Limits, all of which make a check
//! fail (never pass) when they bite: functions produced by macros, K&R-style
//! definitions, and unbalanced braces are not recognised.

use std::ops::Range;

/// `src` with comments and string/char literals replaced by spaces.
/// Newlines are kept, so offsets and line numbers are unchanged.
pub fn blank_comments_and_strings(src: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        Str,
        Char,
    }
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut state = State::Code;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        let blank = |b: u8| if b == b'\n' { b'\n' } else { b' ' };
        match state {
            State::Code => match (b, next) {
                (b'/', Some(b'/')) => {
                    state = State::LineComment;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                (b'/', Some(b'*')) => {
                    state = State::BlockComment;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                (b'"', _) => {
                    state = State::Str;
                    out.push(b' ');
                }
                (b'\'', _) => {
                    state = State::Char;
                    out.push(b' ');
                }
                _ => out.push(b),
            },
            State::LineComment => {
                if b == b'\n' {
                    state = State::Code;
                }
                out.push(blank(b));
            }
            State::BlockComment => {
                if b == b'*' && next == Some(b'/') {
                    state = State::Code;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                out.push(blank(b));
            }
            State::Str | State::Char => {
                let quote = if state == State::Str { b'"' } else { b'\'' };
                if b == b'\\' && next.is_some() {
                    out.push(b' ');
                    out.push(blank(next.unwrap_or(b' ')));
                    i += 2;
                    continue;
                }
                if b == quote || b == b'\n' {
                    state = State::Code;
                    out.push(if b == b'\n' { b'\n' } else { b' ' });
                } else {
                    out.push(blank(b));
                }
            }
        }
        i += 1;
    }
    // Only ASCII bytes were replaced, by ASCII bytes; non-ASCII bytes inside
    // comments/strings became spaces, so the result is valid UTF-8.
    String::from_utf8(out).unwrap_or_default()
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Offsets of `ident` as a whole word in `text`.
fn word_positions<'a>(text: &'a str, ident: &'a str) -> impl Iterator<Item = usize> + 'a {
    let bytes = text.as_bytes();
    text.match_indices(ident).map(|(i, _)| i).filter(move |&i| {
        let before = i.checked_sub(1).map(|j| bytes[j]);
        let after = bytes.get(i + ident.len()).copied();
        !before.is_some_and(is_ident_byte) && !after.is_some_and(is_ident_byte)
    })
}

/// Offset of the first non-whitespace byte at or after `i`.
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Offset just past the bracket matching the one at `open`.
fn matching(bytes: &[u8], open: usize, left: u8, right: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, &b) in bytes[open..].iter().enumerate() {
        if b == left {
            depth += 1;
        } else if b == right {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset + 1);
            }
        }
    }
    None
}

/// Byte range of the definition of function `name` in `src`: from the start
/// of the line holding its name to just past its closing brace. `None` if
/// there is no unambiguous top-level definition.
pub fn function_span(src: &str, name: &str) -> Option<Range<usize>> {
    let code = blank_comments_and_strings(src);
    let bytes = code.as_bytes();
    let mut found = None;
    for at in word_positions(&code, name) {
        // Must be at file scope (brace depth 0).
        let depth = bytes[..at].iter().fold(0i64, |d, &b| match b {
            b'{' => d + 1,
            b'}' => d - 1,
            _ => d,
        });
        if depth != 0 {
            continue;
        }
        let paren = skip_ws(bytes, at + name.len());
        if bytes.get(paren) != Some(&b'(') {
            continue;
        }
        let after_params = matching(bytes, paren, b'(', b')')?;
        let brace = skip_ws(bytes, after_params);
        if bytes.get(brace) != Some(&b'{') {
            continue; // a declaration or a call, not a definition
        }
        let end = matching(bytes, brace, b'{', b'}')?;
        let start = code[..at].rfind('\n').map_or(0, |n| n + 1);
        if found.replace(start..end).is_some() {
            return None; // defined twice: ambiguous
        }
    }
    found
}

/// Assertion calls in `src` (`assert(...)`, `__CPROVER_assert(...)`), each
/// with its whitespace collapsed, in order.
pub fn assertions(src: &str) -> Vec<String> {
    let code = blank_comments_and_strings(src);
    calls(src, &code, "assert")
        .chain(calls(src, &code, "__CPROVER_assert"))
        .collect()
}

/// Number of `name(` occurrences in `src` (calls, and a definition if there
/// is one), ignoring comments and strings.
pub fn count_calls(src: &str, name: &str) -> usize {
    let code = blank_comments_and_strings(src);
    calls(src, &code, name).count()
}

/// The text of every `name(...)` call (from the original `src`, so string
/// arguments such as assertion messages are kept), whitespace collapsed.
fn calls<'a>(src: &'a str, code: &'a str, name: &'a str) -> impl Iterator<Item = String> + 'a {
    let bytes = code.as_bytes();
    word_positions(code, name).filter_map(move |at| {
        let paren = skip_ws(bytes, at + name.len());
        if bytes.get(paren) != Some(&b'(') {
            return None;
        }
        let end = matching(bytes, paren, b'(', b')')?;
        Some(
            src[at..end]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        )
    })
}

/// Preprocessor lines (`#include`, `#define`, `#pragma`, ...), trimmed, in
/// order. Lines inside comments are ignored.
pub fn preprocessor_lines(src: &str) -> Vec<String> {
    let code = blank_comments_and_strings(src);
    code.lines()
        .zip(src.lines())
        .filter(|(c, _)| c.trim_start().starts_with('#'))
        .map(|(_, line)| line.trim().to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"/* header: assert(0) in a comment */
#include <stdint.h>
int helper(int x) { return x + 1; }  // helper(
int target(int a, int b);
int target(int a,
           int b) {
  const char *s = "} not a brace { assert(1) ";
  if (a > b) { a = b; }
  __CPROVER_assert(a <= b, "REQ-1: a not above b");
  assert( a >= 0 );
  return a;
}
"#;

    #[test]
    fn blanking_keeps_offsets_and_newlines() {
        let code = blank_comments_and_strings(SRC);
        assert_eq!(code.len(), SRC.len());
        assert_eq!(code.lines().count(), SRC.lines().count());
        assert!(!code.contains("comment"));
        assert!(!code.contains("not a brace"));
        assert!(code.contains("#include"));
    }

    #[test]
    fn finds_the_definition_not_the_declaration() {
        let span = function_span(SRC, "target").unwrap();
        let text = &SRC[span];
        assert!(text.starts_with("int target(int a,\n"), "{text}");
        assert!(text.ends_with("return a;\n}"), "{text}");
        let helper = &SRC[function_span(SRC, "helper").unwrap()];
        assert_eq!(helper, "int helper(int x) { return x + 1; }");
        assert!(function_span(SRC, "missing").is_none());
        assert!(
            function_span(SRC, "a").is_none(),
            "a variable is not a function"
        );
    }

    #[test]
    fn duplicate_definitions_are_ambiguous() {
        let src = "int f(void) { return 0; }\nint f(void) { return 1; }\n";
        assert!(function_span(src, "f").is_none());
    }

    #[test]
    fn assertions_keep_messages_and_skip_comments_and_strings() {
        assert_eq!(
            assertions(SRC),
            [
                "assert( a >= 0 )",
                "__CPROVER_assert(a <= b, \"REQ-1: a not above b\")"
            ]
        );
        assert_eq!(
            count_calls(SRC, "helper"),
            1,
            "the definition; not the comment"
        );
        assert_eq!(count_calls("x = helper(1) + helper (2);", "helper"), 2);
        assert_eq!(
            count_calls("__CPROVER_assume(x > 0);", "__CPROVER_assume"),
            1
        );
    }

    #[test]
    fn preprocessor_lines_ignore_comments() {
        let src = "#include <a.h>\n/*\n#define X 1\n*/\n  #define Y 2\nint y;\n";
        assert_eq!(preprocessor_lines(src), ["#include <a.h>", "#define Y 2"]);
    }
}
