//! Source formatter behind `aster fmt` (TOOL-001).
//!
//! There is exactly one style and no options. The formatter is token based:
//! it lexes the source, recovers comments from the gaps between token spans
//! (the lexer discards comment trivia, but token spans are exact), and then
//! re-emits the same token sequence with canonical whitespace. Because only
//! whitespace between tokens is ever rewritten, the token stream (excluding
//! whitespace and comments) is unchanged by construction, and the result is
//! re-lexed and compared before it is returned.
//!
//! # Style
//!
//! * Four-space indentation, no tabs, no trailing whitespace, LF line endings
//!   (CRLF is kept when the input already uses it) and one final newline.
//! * Allman braces: the `{` of a multi-line block, type, namespace, property
//!   accessor or `else` goes on its own line, and `else` starts a new line
//!   after a multi-line `}`. Blocks written on one line (`{ return x; }`),
//!   object initializers and switch-expression bodies keep their line.
//! * `case`/`default` labels sit at the `switch` body level and their
//!   statements are indented one level further.
//! * A statement continued on a following line is indented one extra level.
//! * Single spaces around binary operators, `=`, `=>`, ternary `?`/`:`; one
//!   space after `,` and `;`; none inside parentheses or brackets; one space
//!   after control keywords (`if (`); none before call parentheses, indexers
//!   and generic arguments; no space after a cast or a prefix operator.
//! * Existing line breaks are kept; runs of blank lines collapse to one and
//!   blank lines directly inside a brace pair are removed.
//! * Comments are kept verbatim. Own-line comments are re-indented, a
//!   trailing comment is separated by one space.

use aster_diagnostics::Diagnostic;

use crate::{TokenKind, lex};

/// Why a source file could not be formatted.
#[derive(Debug)]
pub enum FormatError {
    /// The source does not lex; it is reported, never rewritten.
    Lex(Vec<Diagnostic>),
    /// Internal safeguard: the formatted text would not lex to the same
    /// tokens. The source is left untouched.
    Unsafe(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lex(diagnostics) => {
                write!(
                    formatter,
                    "source has {} lexical error(s)",
                    diagnostics.len()
                )
            }
            Self::Unsafe(reason) => {
                write!(formatter, "formatting would change the program: {reason}")
            }
        }
    }
}

impl std::error::Error for FormatError {}

/// Format Aster source text with the single canonical style.
///
/// # Errors
///
/// Returns [`FormatError::Lex`] when the source does not lex, and
/// [`FormatError::Unsafe`] if the safeguard re-lex of the result differs from
/// the original token stream.
pub fn format_source(source: &str) -> Result<String, FormatError> {
    let tokens = lex(source).map_err(FormatError::Lex)?;
    let atoms = build_atoms(source, &tokens);
    let items = build_items(source, &atoms)
        .ok_or_else(|| FormatError::Unsafe("unrecognized text between tokens".to_owned()))?;
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let output = Printer::new(&atoms, &items, newline).print();
    let relexed =
        lex(&output).map_err(|_| FormatError::Unsafe("output does not lex".to_owned()))?;
    let before: Vec<&TokenKind> = tokens.iter().map(|token| &token.kind).collect();
    let after: Vec<&TokenKind> = relexed.iter().map(|token| &token.kind).collect();
    if before != after {
        return Err(FormatError::Unsafe("token stream differs".to_owned()));
    }
    Ok(output)
}

struct Atom<'a> {
    kind: TokenKind,
    text: &'a str,
    start: usize,
    end: usize,
}

fn build_atoms<'a>(source: &'a str, tokens: &[crate::Token]) -> Vec<Atom<'a>> {
    let mut atoms = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if token.kind == TokenKind::Eof {
            break;
        }
        if token.kind == TokenKind::InterpolatedStringStart {
            let start = token.span.start;
            let mut depth = 0usize;
            let mut end = token.span.end;
            while index < tokens.len() {
                match tokens[index].kind {
                    TokenKind::InterpolatedStringStart => depth += 1,
                    TokenKind::InterpolatedStringEnd => depth -= 1,
                    _ => {}
                }
                end = tokens[index].span.end;
                index += 1;
                if depth == 0 {
                    break;
                }
            }
            atoms.push(Atom {
                kind: TokenKind::InterpolatedStringStart,
                text: &source[start..end],
                start,
                end,
            });
            continue;
        }
        atoms.push(Atom {
            kind: token.kind.clone(),
            text: &source[token.span.start..token.span.end],
            start: token.span.start,
            end: token.span.end,
        });
        index += 1;
    }
    atoms
}

enum Elem<'a> {
    Atom(usize),
    Comment { text: &'a str, line: bool },
}

struct Item<'a> {
    elem: Elem<'a>,
    /// Newlines in the source between the previous element and this one.
    newlines: usize,
}

/// Interleave atoms with the comments found in the gaps between them.
fn build_items<'a>(source: &'a str, atoms: &[Atom<'a>]) -> Option<Vec<Item<'a>>> {
    let mut items = Vec::new();
    let mut cursor = 0;
    let mut pending = 0;
    for (index, atom) in atoms.iter().enumerate() {
        pending = scan_gap(&source[cursor..atom.start], pending, &mut items)?;
        items.push(Item {
            elem: Elem::Atom(index),
            newlines: pending,
        });
        pending = 0;
        cursor = atom.end;
    }
    scan_gap(&source[cursor..], pending, &mut items)?;
    Some(items)
}

/// Split a gap (whitespace and comments only) into comment items. Returns the
/// number of newlines after the last comment, to be credited to the next item.
fn scan_gap<'a>(gap: &'a str, mut newlines: usize, items: &mut Vec<Item<'a>>) -> Option<usize> {
    let mut position = 0;
    while position < gap.len() {
        let rest = &gap[position..];
        if rest.starts_with("//") {
            let length = rest.find('\n').unwrap_or(rest.len());
            items.push(Item {
                elem: Elem::Comment {
                    text: rest[..length].trim_end(),
                    line: true,
                },
                newlines,
            });
            newlines = 0;
            position += length;
        } else if rest.starts_with("/*") {
            let length = rest.find("*/")? + 2;
            items.push(Item {
                elem: Elem::Comment {
                    text: &rest[..length],
                    line: false,
                },
                newlines,
            });
            newlines = 0;
            position += length;
        } else {
            let character = rest.chars().next()?;
            if !character.is_whitespace() {
                return None;
            }
            if character == '\n' {
                newlines += 1;
            }
            position += character.len_utf8();
        }
    }
    Some(newlines)
}

// ---------------------------------------------------------------------------
// Token classification
// ---------------------------------------------------------------------------

fn is_type_keyword(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Bool
            | TokenKind::SByte
            | TokenKind::Byte
            | TokenKind::Short
            | TokenKind::UShort
            | TokenKind::Int
            | TokenKind::UInt
            | TokenKind::Long
            | TokenKind::ULong
            | TokenKind::Float
            | TokenKind::Double
            | TokenKind::Decimal
            | TokenKind::Char
            | TokenKind::String
    )
}

fn is_literal(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::IntegerLiteral(_)
            | TokenKind::LongLiteral(_)
            | TokenKind::UIntLiteral(_)
            | TokenKind::ULongLiteral(_)
            | TokenKind::FloatLiteral(_)
            | TokenKind::DoubleLiteral(_)
            | TokenKind::DecimalLiteral(_)
            | TokenKind::StringLiteral(_)
            | TokenKind::CharacterLiteral(_)
            | TokenKind::InterpolatedStringStart
    )
}

fn is_opener(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::LeftBrace | TokenKind::LeftParen | TokenKind::LeftBracket
    )
}

fn is_closer(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::RightBrace | TokenKind::RightParen | TokenKind::RightBracket
    )
}

fn is_control_keyword(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::If
            | TokenKind::While
            | TokenKind::For
            | TokenKind::Foreach
            | TokenKind::Switch
            | TokenKind::Case
            | TokenKind::Using
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColonKind {
    Ternary,
    Inherit,
    Ctor,
    Case,
    Label,
    Named,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // Independent per-token role flags.
struct Roles {
    generic_open: bool,
    generic_close: bool,
    ternary: bool,
    colon: Option<ColonKind>,
    prefix: bool,
    postfix: bool,
    cast_close: bool,
    /// Opener/closer partner.
    partner: Option<usize>,
}

fn classify(atoms: &[Atom<'_>]) -> Vec<Roles> {
    let mut roles: Vec<Roles> = atoms.iter().map(|_| Roles::default()).collect();
    match_brackets(atoms, &mut roles);
    mark_generics(atoms, &mut roles);
    mark_casts(atoms, &mut roles);
    mark_ternaries(atoms, &mut roles);
    mark_colons(atoms, &mut roles);
    mark_unary(atoms, &mut roles);
    roles
}

fn match_brackets(atoms: &[Atom<'_>], roles: &mut [Roles]) {
    let mut stack: Vec<usize> = Vec::new();
    for (index, atom) in atoms.iter().enumerate() {
        if is_opener(&atom.kind) {
            stack.push(index);
        } else if is_closer(&atom.kind)
            && let Some(open) = stack.pop()
        {
            roles[open].partner = Some(index);
            roles[index].partner = Some(open);
        }
    }
}

fn mark_generics(atoms: &[Atom<'_>], roles: &mut [Roles]) {
    for start in 1..atoms.len() {
        if atoms[start].kind != TokenKind::Less
            || !matches!(atoms[start - 1].kind, TokenKind::Identifier(_))
        {
            continue;
        }
        let mut level = 0i32;
        let mut close = None;
        for (offset, atom) in atoms[start..].iter().enumerate() {
            match &atom.kind {
                TokenKind::Less => level += 1,
                TokenKind::Greater => {
                    level -= 1;
                    if level == 0 {
                        close = Some(start + offset);
                        break;
                    }
                }
                TokenKind::Identifier(_)
                | TokenKind::Comma
                | TokenKind::Dot
                | TokenKind::LeftBracket
                | TokenKind::RightBracket
                | TokenKind::Question => {}
                kind if is_type_keyword(kind) => {}
                _ => break,
            }
        }
        let Some(close) = close else { continue };
        let follows = atoms.get(close + 1).is_none_or(|next| {
            matches!(
                next.kind,
                TokenKind::LeftParen
                    | TokenKind::RightParen
                    | TokenKind::RightBracket
                    | TokenKind::RightBrace
                    | TokenKind::Colon
                    | TokenKind::Semicolon
                    | TokenKind::Comma
                    | TokenKind::Dot
                    | TokenKind::Question
                    | TokenKind::LeftBracket
                    | TokenKind::LeftBrace
                    | TokenKind::Identifier(_)
                    | TokenKind::Greater
                    | TokenKind::Equal
                    | TokenKind::EqualEqual
                    | TokenKind::BangEqual
            ) || is_type_keyword(&next.kind)
        });
        if follows {
            roles[start].generic_open = true;
            roles[close].generic_close = true;
        }
    }
}

fn mark_casts(atoms: &[Atom<'_>], roles: &mut [Roles]) {
    for close in 0..atoms.len() {
        if atoms[close].kind != TokenKind::RightParen {
            continue;
        }
        let Some(open) = roles[close].partner else {
            continue;
        };
        // A parenthesis that follows a callee or a control keyword is a call
        // or a condition, never a cast.
        if open > 0 {
            let before = &atoms[open - 1];
            if matches!(
                before.kind,
                TokenKind::Identifier(_)
                    | TokenKind::RightParen
                    | TokenKind::RightBracket
                    | TokenKind::New
                    | TokenKind::Base
                    | TokenKind::This
                    | TokenKind::Default
            ) || is_type_keyword(&before.kind)
                || is_control_keyword(&before.kind)
                || roles[open - 1].generic_close
            {
                continue;
            }
        }
        let content = &atoms[open + 1..close];
        let primitive = content
            .first()
            .is_some_and(|atom| is_type_keyword(&atom.kind))
            && content[1..].iter().all(|atom| {
                matches!(
                    atom.kind,
                    TokenKind::LeftBracket | TokenKind::RightBracket | TokenKind::Question
                )
            });
        let named = content.len() % 2 == 1
            && content.iter().enumerate().all(|(position, atom)| {
                if position % 2 == 0 {
                    matches!(atom.kind, TokenKind::Identifier(_))
                } else {
                    atom.kind == TokenKind::Dot
                }
            })
            && atoms.get(close + 1).is_some_and(|next| {
                matches!(
                    next.kind,
                    TokenKind::Identifier(_)
                        | TokenKind::This
                        | TokenKind::New
                        | TokenKind::True
                        | TokenKind::False
                ) || is_literal(&next.kind)
            });
        roles[close].cast_close = primitive || named;
    }
}

fn mark_ternaries(atoms: &[Atom<'_>], roles: &mut [Roles]) {
    for start in 0..atoms.len() {
        if atoms[start].kind != TokenKind::Question {
            continue;
        }
        let mut depth = 0i32;
        let mut nested = 0i32;
        for (offset, atom) in atoms[start + 1..].iter().enumerate() {
            let position = start + 1 + offset;
            match atom.kind {
                TokenKind::LeftParen | TokenKind::LeftBracket | TokenKind::LeftBrace => depth += 1,
                TokenKind::RightParen | TokenKind::RightBracket | TokenKind::RightBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                TokenKind::Semicolon | TokenKind::Comma if depth == 0 => break,
                TokenKind::Question if depth == 0 => nested += 1,
                TokenKind::Colon if depth == 0 => {
                    if nested == 0 {
                        roles[start].ternary = true;
                        roles[position].colon = Some(ColonKind::Ternary);
                        break;
                    }
                    nested -= 1;
                }
                _ => {}
            }
        }
    }
}

fn mark_colons(atoms: &[Atom<'_>], roles: &mut [Roles]) {
    let mut pending_case = false;
    let mut declaration_header = false;
    for index in 0..atoms.len() {
        match &atoms[index].kind {
            TokenKind::Case => pending_case = true,
            TokenKind::FatArrow | TokenKind::Semicolon | TokenKind::LeftBrace => {
                pending_case = false;
                declaration_header = false;
            }
            // A `where T : Constraint` clause spaces its colon like inheritance.
            TokenKind::Identifier(name) if name == "where" => declaration_header = true,
            TokenKind::Class | TokenKind::Struct | TokenKind::Interface | TokenKind::Enum => {
                declaration_header = true;
            }
            TokenKind::Colon if roles[index].colon.is_none() => {
                let previous = index.checked_sub(1).map(|before| &atoms[before].kind);
                roles[index].colon = Some(if pending_case {
                    pending_case = false;
                    ColonKind::Case
                } else if declaration_header {
                    ColonKind::Inherit
                } else if previous == Some(&TokenKind::Default) {
                    ColonKind::Label
                } else if previous == Some(&TokenKind::RightParen) {
                    ColonKind::Ctor
                } else {
                    ColonKind::Named
                });
            }
            _ => {}
        }
    }
}

fn mark_unary(atoms: &[Atom<'_>], roles: &mut [Roles]) {
    for index in 0..atoms.len() {
        let operand_before = index > 0 && ends_operand(&atoms[index - 1].kind, &roles[index - 1]);
        match atoms[index].kind {
            TokenKind::Bang | TokenKind::Tilde => roles[index].prefix = true,
            TokenKind::Plus | TokenKind::Minus => roles[index].prefix = !operand_before,
            TokenKind::PlusPlus | TokenKind::MinusMinus => {
                if operand_before {
                    roles[index].postfix = true;
                } else {
                    roles[index].prefix = true;
                }
            }
            TokenKind::Question => roles[index].postfix = !roles[index].ternary,
            _ => {}
        }
    }
}

fn ends_operand(kind: &TokenKind, roles: &Roles) -> bool {
    match kind {
        TokenKind::Identifier(_)
        | TokenKind::RightBracket
        | TokenKind::This
        | TokenKind::Base
        | TokenKind::True
        | TokenKind::False => true,
        TokenKind::RightParen => !roles.cast_close,
        TokenKind::Greater => roles.generic_close,
        TokenKind::PlusPlus | TokenKind::MinusMinus | TokenKind::Question => roles.postfix,
        other => is_literal(other),
    }
}

// ---------------------------------------------------------------------------
// Spacing between two adjacent atoms on one line
// ---------------------------------------------------------------------------

fn space_between(atoms: &[Atom<'_>], roles: &[Roles], next: usize) -> bool {
    let previous = next - 1;
    let (p, n) = (&atoms[previous], &atoms[next]);
    // `>>>` and friends: keep source-adjacent `>` runs glued.
    if p.kind == TokenKind::Greater
        && matches!(n.kind, TokenKind::Greater | TokenKind::GreaterEqual)
        && p.end == n.start
    {
        return false;
    }
    let both_generic_close = roles[previous].generic_close && roles[next].generic_close;
    let decided = decide_space(atoms, roles, previous, next);
    if !decided && !both_generic_close && merges(p.text, n.text) {
        return true;
    }
    decided
}

/// Would two glued texts risk lexing differently (operator characters)?
fn merges(left: &str, right: &str) -> bool {
    const OPERATOR_CHARACTERS: &str = "+-*/%&|^<>=!~?:";
    left.chars()
        .next_back()
        .is_some_and(|c| OPERATOR_CHARACTERS.contains(c))
        && right
            .chars()
            .next()
            .is_some_and(|c| OPERATOR_CHARACTERS.contains(c))
}

fn glues_before_open(kind: &TokenKind, roles: &Roles, for_paren: bool) -> bool {
    match kind {
        TokenKind::Identifier(_)
        | TokenKind::RightBracket
        | TokenKind::This
        | TokenKind::RightParen => true,
        TokenKind::Greater => roles.generic_close,
        TokenKind::New | TokenKind::Base | TokenKind::Default => for_paren,
        other => is_type_keyword(other),
    }
}

fn decide_space(atoms: &[Atom<'_>], roles: &[Roles], previous: usize, next: usize) -> bool {
    let (pk, nk) = (&atoms[previous].kind, &atoms[next].kind);
    let (pr, nr) = (&roles[previous], &roles[next]);
    if matches!(
        pk,
        TokenKind::LeftParen | TokenKind::LeftBracket | TokenKind::Dot
    ) {
        return false;
    }
    if matches!(
        nk,
        TokenKind::RightParen
            | TokenKind::RightBracket
            | TokenKind::Comma
            | TokenKind::Semicolon
            | TokenKind::Dot
    ) {
        return false;
    }
    if *pk == TokenKind::LeftBrace && *nk == TokenKind::RightBrace {
        return false;
    }
    if pr.prefix
        || pr.cast_close
        || pr.generic_open
        || nr.postfix
        || nr.generic_open
        || nr.generic_close
    {
        return false;
    }
    match nk {
        TokenKind::LeftParen => !glues_before_open(pk, pr, true),
        TokenKind::LeftBracket => !glues_before_open(pk, pr, false),
        TokenKind::Colon => matches!(
            nr.colon,
            Some(ColonKind::Ternary | ColonKind::Inherit | ColonKind::Ctor)
        ),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Printer
// ---------------------------------------------------------------------------

struct Open {
    kind: TokenKind,
    case_mode: bool,
}

struct Printer<'a> {
    atoms: &'a [Atom<'a>],
    items: &'a [Item<'a>],
    newline: &'a str,
    roles: Vec<Roles>,
    /// Source line of each atom's first byte.
    lines: Vec<usize>,
    /// First atom on the same source line as each atom.
    line_first: Vec<usize>,
}

impl<'a> Printer<'a> {
    fn new(atoms: &'a [Atom<'a>], items: &'a [Item<'a>], newline: &'a str) -> Self {
        // Lines are derived from the item newline counts.
        let mut lines = vec![0; atoms.len()];
        let mut line = 0usize;
        for item in items {
            line += item.newlines;
            match item.elem {
                Elem::Atom(index) => {
                    lines[index] = line;
                    line += atoms[index].text.matches('\n').count();
                }
                Elem::Comment { text, .. } => line += text.matches('\n').count(),
            }
        }
        let mut line_first = vec![0; atoms.len()];
        for index in 0..atoms.len() {
            line_first[index] = if index > 0 && lines[index - 1] == lines[index] {
                line_first[index - 1]
            } else {
                index
            };
        }
        Self {
            atoms,
            items,
            newline,
            roles: classify(atoms),
            lines,
            line_first,
        }
    }

    fn kind(&self, index: usize) -> &TokenKind {
        &self.atoms[index].kind
    }

    fn next_atom_after(&self, item: usize) -> Option<usize> {
        self.items[item + 1..]
            .iter()
            .find_map(|next| match next.elem {
                Elem::Atom(index) => Some(index),
                Elem::Comment { .. } => None,
            })
    }

    fn is_label(&self, index: usize) -> bool {
        match self.kind(index) {
            TokenKind::Case => true,
            TokenKind::Default => self
                .atoms
                .get(index + 1)
                .is_some_and(|next| next.kind == TokenKind::Colon),
            _ => false,
        }
    }

    /// Should the `{` at `index` (currently trailing its line) move to its own line?
    fn moves_to_own_line(&self, item: usize, index: usize) -> bool {
        if index == 0 {
            return false;
        }
        let previous = index - 1;
        // `{` must be the last thing on its line and its block multi-line.
        let trailing = self
            .items
            .get(item + 1)
            .is_some_and(|next| next.newlines > 0);
        let multiline = self.roles[index]
            .partner
            .is_some_and(|close| self.lines[close] > self.lines[index]);
        if !trailing || !multiline {
            return false;
        }
        match self.kind(previous) {
            TokenKind::RightParen | TokenKind::Else | TokenKind::Do => true,
            TokenKind::Identifier(_) | TokenKind::Greater => self.declares(previous, index),
            _ => false,
        }
    }

    /// Is the `{` at `brace` the body of a declaration or accessor (as
    /// opposed to an object initializer or expression body)?
    fn declares(&self, previous: usize, brace: usize) -> bool {
        let first = self.line_first[brace];
        let accessor = matches!(
            &self.atoms[previous].kind,
            TokenKind::Identifier(name) if name == "get" || name == "set" || name == "init"
        );
        let declaration_start = matches!(
            self.kind(first),
            TokenKind::Public
                | TokenKind::Internal
                | TokenKind::Protected
                | TokenKind::Private
                | TokenKind::Static
                | TokenKind::Async
                | TokenKind::Class
                | TokenKind::Struct
                | TokenKind::Interface
                | TokenKind::Enum
                | TokenKind::Namespace
        );
        (accessor || declaration_start)
            && !self.atoms[first..brace].iter().any(|atom| {
                matches!(
                    atom.kind,
                    TokenKind::Equal | TokenKind::FatArrow | TokenKind::New | TokenKind::Return
                )
            })
    }

    #[allow(clippy::too_many_lines)] // One pass: newlines, indentation, spacing.
    fn print(&self) -> String {
        let mut out = String::new();
        let mut stack: Vec<Open> = Vec::new();
        let mut previous_atom: Option<usize> = None;
        // `Some(is_line_comment)` when the previous element was a comment.
        let mut previous_comment: Option<bool> = None;
        let mut last_atom: Option<usize> = None;
        let mut closed_multiline = false;
        for (position, item) in self.items.iter().enumerate() {
            let mut newlines = item.newlines;
            if previous_comment == Some(true) {
                newlines = newlines.max(1);
            }
            let current_atom = match item.elem {
                Elem::Atom(index) => Some(index),
                Elem::Comment { .. } => None,
            };
            if let (Some(index), Some(before), 0) = (current_atom, previous_atom, newlines) {
                let kind = self.kind(index);
                let brace_on_own_line =
                    *kind == TokenKind::LeftBrace && self.moves_to_own_line(position, index);
                let else_after_block = *kind == TokenKind::Else
                    && *self.kind(before) == TokenKind::RightBrace
                    && closed_multiline;
                if brace_on_own_line || else_after_block {
                    newlines = 1;
                }
            }
            // Closers leave their scope before their indentation is computed.
            if let Some(index) = current_atom
                && is_closer(self.kind(index))
            {
                stack.pop();
            }
            if position == 0 {
                // No leading blank lines or indentation.
            } else if newlines > 0 {
                let closer_next = current_atom.is_some_and(|index| is_closer(self.kind(index)));
                let after_opener = previous_atom.is_some_and(|index| is_opener(self.kind(index)));
                let count = if closer_next || after_opener {
                    1
                } else {
                    newlines.min(2)
                };
                for _ in 0..count {
                    out.push_str(self.newline);
                }
                for _ in 0..self.indent_for(position, current_atom, &stack, last_atom) {
                    out.push_str("    ");
                }
            } else {
                let space = match (current_atom, previous_atom) {
                    (Some(index), Some(_)) => space_between(self.atoms, &self.roles, index),
                    _ => true,
                };
                if space {
                    out.push(' ');
                }
            }
            match item.elem {
                Elem::Comment { text, line } => {
                    out.push_str(text);
                    previous_comment = Some(line);
                    previous_atom = None;
                }
                Elem::Atom(index) => {
                    let atom = &self.atoms[index];
                    out.push_str(atom.text);
                    if atom.kind == TokenKind::RightBrace {
                        closed_multiline = self.roles[index]
                            .partner
                            .is_some_and(|open| self.lines[open] < self.lines[index]);
                    }
                    if is_opener(&atom.kind) {
                        stack.push(Open {
                            kind: atom.kind.clone(),
                            case_mode: false,
                        });
                    }
                    if self.is_label(index)
                        && let Some(top) = stack.last_mut()
                        && top.kind == TokenKind::LeftBrace
                    {
                        top.case_mode = true;
                    }
                    previous_atom = Some(index);
                    previous_comment = None;
                    last_atom = Some(index);
                }
            }
        }
        // Exactly one final newline (none for an empty file).
        let trimmed = out.trim_end_matches([' ', '\r', '\n']).len();
        out.truncate(trimmed);
        if !out.is_empty() {
            out.push_str(self.newline);
        }
        out
    }

    fn indent_for(
        &self,
        position: usize,
        atom: Option<usize>,
        stack: &[Open],
        last_atom: Option<usize>,
    ) -> usize {
        let mut depth: usize = stack
            .iter()
            .map(|open| 1 + usize::from(open.case_mode))
            .sum();
        // A comment is placed like the atom that follows it.
        let decider = atom.or_else(|| self.next_atom_after(position));
        let label = decider.is_some_and(|index| self.is_label(index));
        if label
            && let Some(top) = stack.last()
            && top.kind == TokenKind::LeftBrace
        {
            depth -= usize::from(top.case_mode);
        }
        if let Some(index) = atom {
            let kind = self.kind(index);
            let top_is_block = stack
                .last()
                .is_none_or(|open| open.kind == TokenKind::LeftBrace);
            let continues = last_atom.is_some_and(|before| {
                !matches!(
                    self.kind(before),
                    TokenKind::Semicolon
                        | TokenKind::LeftBrace
                        | TokenKind::RightBrace
                        | TokenKind::Comma
                        | TokenKind::Colon
                )
            });
            if top_is_block
                && continues
                && !is_closer(kind)
                && *kind != TokenKind::LeftBrace
                && !label
            {
                depth += 1;
            }
        }
        depth
    }
}
