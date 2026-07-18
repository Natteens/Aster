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
        let location = locate(source, self.span.start);
        let line_text = source
            .lines()
            .nth(location.line.saturating_sub(1))
            .unwrap_or("");
        let width = self.span.end.saturating_sub(self.span.start).max(1);
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
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..offset].chars().count() + 1;
    Location { line, column }
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
}
