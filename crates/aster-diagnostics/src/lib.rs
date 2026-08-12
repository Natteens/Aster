//! Source spans and diagnostics shared by the Aster front-end.

use std::fmt::Write;

/// A half-open byte range in a UTF-8 source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// A source position suitable for display to users.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

/// The severity of a compile-time diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A compile-time diagnostic tied to source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    pub help: Option<String>,
}

impl Diagnostic {
    #[must_use]
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            span,
            help: None,
        }
    }

    #[must_use]
    pub fn warning(message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            span,
            help: None,
        }
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Render a compact rustc-style diagnostic with a source excerpt.
    #[must_use]
    pub fn render(&self, file_name: &str, source: &str) -> String {
        let span_start = char_boundary_at_or_before(source, self.span.start);
        let span_end = char_boundary_at_or_before(source, self.span.end);
        let location = locate(source, span_start);
        let line_text = source
            .lines()
            .nth(location.line.saturating_sub(1))
            .unwrap_or("");
        let width = source
            .get(span_start..span_end)
            .and_then(|text| text.lines().next())
            .map(str::chars)
            .map_or(0, Iterator::count)
            .max(1);
        let marker = format!("{}{}", " ".repeat(location.column - 1), "^".repeat(width));
        let gutter = location.line.to_string().len();
        let mut output = String::new();
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        writeln!(output, "{severity}: {}", self.message).expect("writing to a String cannot fail");
        writeln!(
            output,
            " --> {file_name}:{}:{}",
            location.line, location.column
        )
        .expect("writing to a String cannot fail");
        writeln!(output, "{} |", " ".repeat(gutter)).expect("writing to a String cannot fail");
        writeln!(output, "{} | {line_text}", location.line)
            .expect("writing to a String cannot fail");
        write!(output, "{} | {marker}", " ".repeat(gutter))
            .expect("writing to a String cannot fail");
        if let Some(help) = &self.help {
            write!(output, "\nhelp: {help}").expect("writing to a String cannot fail");
        }
        output
    }
}

/// Translate a byte offset into a one-based line and Unicode-scalar column.
#[must_use]
pub fn locate(source: &str, offset: usize) -> Location {
    let offset = char_boundary_at_or_before(source, offset);
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..offset].chars().count() + 1;
    Location { line, column }
}

fn char_boundary_at_or_before(source: &str, offset: usize) -> usize {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, Location, Span, locate};

    #[test]
    fn finds_unicode_location() {
        assert_eq!(locate("αβ\nvalue", 4), Location { line: 1, column: 3 });
        assert_eq!(locate("αβ\nvalue", 5), Location { line: 2, column: 1 });
    }

    #[test]
    fn renders_line_and_column() {
        let diagnostic = Diagnostic::error("expected a name", Span::new(10, 11));
        let rendered = diagnostic.render("sample.aster", "component {\n}");
        assert!(rendered.contains("sample.aster:1:11"));
        assert!(rendered.contains('^'));
    }

    #[test]
    fn arbitrary_byte_offsets_do_not_panic_inside_unicode_scalars() {
        assert_eq!(locate("é", 1), Location { line: 1, column: 1 });
        let rendered = Diagnostic::error("bad span", Span::new(1, 1)).render("sample.aster", "é");
        assert!(rendered.contains("sample.aster:1:1"));
    }

    #[test]
    fn marker_width_counts_unicode_scalars_and_stops_at_the_line_boundary() {
        let unicode =
            Diagnostic::error("unicode", Span::new(0, "é".len())).render("sample.aster", "é");
        assert!(unicode.ends_with("  | ^"), "{unicode}");

        let multiline =
            Diagnostic::error("multiline", Span::new(0, 5)).render("sample.aster", "ab\ncd");
        assert!(multiline.ends_with("  | ^^"), "{multiline}");
    }
}
