use aster_diagnostics::{Diagnostic, Span};

use crate::{Token, TokenKind};

/// Convert source text into positioned tokens.
///
/// # Errors
///
/// Returns diagnostics when the input contains malformed or unsupported tokens.
pub fn lex(source: &str) -> Result<Vec<Token>, Vec<Diagnostic>> {
    let mut lexer = Lexer {
        source,
        cursor: 0,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
    };
    lexer.run();
    if lexer.diagnostics.is_empty() {
        Ok(lexer.tokens)
    } else {
        Err(lexer.diagnostics)
    }
}

struct Lexer<'a> {
    source: &'a str,
    cursor: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.cursor < self.source.len() {
            let start = self.cursor;
            let character = self.bump().expect("cursor is within source");
            match character {
                c if c.is_whitespace() => {}
                '/' if self.peek() == Some('/') => self.skip_line_comment(),
                c if c == '_' || c.is_alphabetic() => self.identifier(start),
                c if c.is_ascii_digit() => self.number(start),
                '"' => self.string(start),
                '\'' => self.character(start),
                '{' => self.token(TokenKind::LeftBrace, start),
                '}' => self.token(TokenKind::RightBrace, start),
                '(' => self.token(TokenKind::LeftParen, start),
                ')' => self.token(TokenKind::RightParen, start),
                '[' => self.token(TokenKind::LeftBracket, start),
                ']' => self.token(TokenKind::RightBracket, start),
                ',' => self.token(TokenKind::Comma, start),
                ':' => self.token(TokenKind::Colon, start),
                '?' => self.token(TokenKind::Question, start),
                ';' => self.token(TokenKind::Semicolon, start),
                '.' => self.token(TokenKind::Dot, start),
                '+' => match self.peek() {
                    Some('+') => {
                        self.bump();
                        self.token(TokenKind::PlusPlus, start);
                    }
                    Some('=') => {
                        self.bump();
                        self.token(TokenKind::PlusEqual, start);
                    }
                    _ => self.token(TokenKind::Plus, start),
                },
                '-' => match self.peek() {
                    Some('-') => {
                        self.bump();
                        self.token(TokenKind::MinusMinus, start);
                    }
                    Some('=') => {
                        self.bump();
                        self.token(TokenKind::MinusEqual, start);
                    }
                    _ => self.token(TokenKind::Minus, start),
                },
                '*' => self.two_character(start, '=', TokenKind::StarEqual, TokenKind::Star),
                '/' => self.two_character(start, '=', TokenKind::SlashEqual, TokenKind::Slash),
                '%' => self.token(TokenKind::Percent, start),
                '!' => self.two_character(start, '=', TokenKind::BangEqual, TokenKind::Bang),
                '=' => self.two_character(start, '=', TokenKind::EqualEqual, TokenKind::Equal),
                '<' => self.two_character(start, '=', TokenKind::LessEqual, TokenKind::Less),
                '>' => self.two_character(start, '=', TokenKind::GreaterEqual, TokenKind::Greater),
                '&' if self.peek() == Some('&') => {
                    self.bump();
                    self.token(TokenKind::AndAnd, start);
                }
                '|' if self.peek() == Some('|') => {
                    self.bump();
                    self.token(TokenKind::OrOr, start);
                }
                '&' | '|' => self.diagnostics.push(
                    Diagnostic::error(
                        format!("unexpected operator `{character}`"),
                        Span::new(start, self.cursor),
                    )
                    .with_help(format!(
                        "use `{character}{character}` for a logical operator"
                    )),
                ),
                _ => self.diagnostics.push(Diagnostic::error(
                    format!("unexpected character `{character}`"),
                    Span::new(start, self.cursor),
                )),
            }
        }
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(self.cursor, self.cursor),
        });
    }

    fn identifier(&mut self, start: usize) {
        while self.peek().is_some_and(|c| c == '_' || c.is_alphanumeric()) {
            self.bump();
        }
        let text = &self.source[start..self.cursor];
        let kind = match text {
            "public" => TokenKind::Public,
            "internal" => TokenKind::Internal,
            "protected" => TokenKind::Protected,
            "private" => TokenKind::Private,
            "static" => TokenKind::Static,
            "class" => TokenKind::Class,
            "struct" => TokenKind::Struct,
            "interface" => TokenKind::Interface,
            "enum" => TokenKind::Enum,
            "namespace" => TokenKind::Namespace,
            "using" => TokenKind::Using,
            "module" => TokenKind::Module,
            "import" => TokenKind::Import,
            "const" => TokenKind::Const,
            "var" => TokenKind::Var,
            "void" => TokenKind::Void,
            "bool" => TokenKind::Bool,
            "sbyte" => TokenKind::SByte,
            "byte" => TokenKind::Byte,
            "short" => TokenKind::Short,
            "ushort" => TokenKind::UShort,
            "int" => TokenKind::Int,
            "uint" => TokenKind::UInt,
            "long" => TokenKind::Long,
            "ulong" => TokenKind::ULong,
            "float" => TokenKind::Float,
            "double" => TokenKind::Double,
            "decimal" => TokenKind::Decimal,
            "char" => TokenKind::Char,
            "string" => TokenKind::String,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "switch" => TokenKind::Switch,
            "case" => TokenKind::Case,
            "default" => TokenKind::Default,
            "new" => TokenKind::New,
            "this" => TokenKind::This,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            _ => TokenKind::Identifier(text.to_owned()),
        };
        self.token(kind, start);
    }

    fn number(&mut self, start: usize) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
        }
        let is_float = self.peek() == Some('.')
            && self
                .source
                .get(self.cursor + 1..)
                .and_then(|rest| rest.chars().next())
                .is_some_and(|c| c.is_ascii_digit());
        if is_float {
            self.bump();
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.bump();
            }
        }
        let text = self.source[start..self.cursor].to_owned();
        let kind = match self.peek() {
            Some('L' | 'l' | 'U' | 'u') if is_float => {
                self.bump();
                self.diagnostics.push(
                    Diagnostic::error(
                        "integer suffixes are not valid on literals with a decimal point",
                        Span::new(start, self.cursor),
                    )
                    .with_help("use `f` for `float`, `d` for `double`, or `m` for `decimal`"),
                );
                return;
            }
            Some('L' | 'l') => {
                self.bump();
                if let Some('U' | 'u') = self.peek() {
                    self.bump();
                    TokenKind::ULongLiteral(text)
                } else {
                    TokenKind::LongLiteral(text)
                }
            }
            Some('U' | 'u') => {
                self.bump();
                if let Some('L' | 'l') = self.peek() {
                    self.bump();
                    TokenKind::ULongLiteral(text)
                } else {
                    TokenKind::UIntLiteral(text)
                }
            }
            Some('f' | 'F') => {
                self.bump();
                TokenKind::FloatLiteral(text)
            }
            Some('d' | 'D') => {
                self.bump();
                TokenKind::DoubleLiteral(text)
            }
            Some('m' | 'M') => {
                self.bump();
                TokenKind::DecimalLiteral(text)
            }
            _ if is_float => TokenKind::FloatLiteral(text),
            _ => TokenKind::IntegerLiteral(text),
        };
        self.token(kind, start);
    }

    fn string(&mut self, start: usize) {
        if let Some(value) = self.quoted_value(start, '"') {
            self.token(TokenKind::StringLiteral(value), start);
        }
    }

    fn character(&mut self, start: usize) {
        if let Some(value) = self.quoted_value(start, '\'') {
            let mut characters = value.chars();
            let character = characters.next();
            if let Some(character) = character
                && characters.next().is_none()
            {
                self.token(TokenKind::CharacterLiteral(character), start);
            } else {
                self.diagnostics.push(
                    Diagnostic::error(
                        "character literal must contain exactly one character",
                        Span::new(start, self.cursor),
                    )
                    .with_help("use double quotes for a string literal"),
                );
            }
        }
    }

    fn quoted_value(&mut self, start: usize, delimiter: char) -> Option<String> {
        let mut value = String::new();
        while let Some(character) = self.bump() {
            if character == delimiter {
                return Some(value);
            }
            if character == '\n' || character == '\r' {
                break;
            }
            if character == '\\' {
                let Some(escaped) = self.bump() else {
                    break;
                };
                match escaped {
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    '\\' => value.push('\\'),
                    '"' => value.push('"'),
                    '\'' => value.push('\''),
                    _ => {
                        self.diagnostics.push(
                            Diagnostic::error(
                                format!("unsupported escape sequence `\\{escaped}`"),
                                Span::new(self.cursor - escaped.len_utf8() - 1, self.cursor),
                            )
                            .with_help("use one of `\\n`, `\\r`, `\\t`, `\\\\`, `\\\"`, or `\\'`"),
                        );
                        value.push(escaped);
                    }
                }
            } else {
                value.push(character);
            }
        }
        self.diagnostics.push(
            Diagnostic::error(
                if delimiter == '"' {
                    "unterminated string literal"
                } else {
                    "unterminated character literal"
                },
                Span::new(start, self.cursor),
            )
            .with_help(format!("close the literal with `{delimiter}`")),
        );
        None
    }

    fn two_character(
        &mut self,
        start: usize,
        second: char,
        combined: TokenKind,
        single: TokenKind,
    ) {
        if self.peek() == Some(second) {
            self.bump();
            self.token(combined, start);
        } else {
            self.token(single, start);
        }
    }

    fn skip_line_comment(&mut self) {
        self.bump();
        while self.peek().is_some_and(|character| character != '\n') {
            self.bump();
        }
    }

    fn token(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.cursor),
        });
    }

    fn peek(&self) -> Option<char> {
        self.source[self.cursor..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.cursor += character.len_utf8();
        Some(character)
    }
}

#[cfg(test)]
mod tests {
    use aster_diagnostics::Span;

    use super::lex;
    use crate::TokenKind;

    #[test]
    fn lexes_all_general_keywords() {
        let source = "public internal protected private class struct interface const var void bool int long float double char string return if else while for break continue true false";
        let tokens = lex(source).expect("keywords should lex");
        let kinds = tokens
            .into_iter()
            .map(|token| token.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Public,
                TokenKind::Internal,
                TokenKind::Protected,
                TokenKind::Private,
                TokenKind::Class,
                TokenKind::Struct,
                TokenKind::Interface,
                TokenKind::Const,
                TokenKind::Var,
                TokenKind::Void,
                TokenKind::Bool,
                TokenKind::Int,
                TokenKind::Long,
                TokenKind::Float,
                TokenKind::Double,
                TokenKind::Char,
                TokenKind::String,
                TokenKind::Return,
                TokenKind::If,
                TokenKind::Else,
                TokenKind::While,
                TokenKind::For,
                TokenKind::Break,
                TokenKind::Continue,
                TokenKind::True,
                TokenKind::False,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_namespace_keywords_and_legacy_migration_tokens() {
        let tokens = lex("namespace app; using aster.math; module old; import old;")
            .expect("namespace syntax should lex");
        assert_eq!(tokens[0].kind, TokenKind::Namespace);
        assert_eq!(tokens[3].kind, TokenKind::Using);
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Module));
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Import));
    }

    #[test]
    fn lexes_literals_operators_and_spans() {
        let source = "42 3.14 \"text\" 'x' += != &&";
        let tokens = lex(source).expect("valid tokens");
        assert_eq!(tokens[0].kind, TokenKind::IntegerLiteral("42".into()));
        assert_eq!(tokens[0].span, Span::new(0, 2));
        assert_eq!(tokens[1].kind, TokenKind::FloatLiteral("3.14".into()));
        assert_eq!(tokens[2].kind, TokenKind::StringLiteral("text".into()));
        assert_eq!(tokens[3].kind, TokenKind::CharacterLiteral('x'));
        assert_eq!(tokens[4].kind, TokenKind::PlusEqual);
        assert_eq!(tokens[5].kind, TokenKind::BangEqual);
        assert_eq!(tokens[6].kind, TokenKind::AndAnd);
    }

    #[test]
    fn lexes_increment_decrement_and_question() {
        let tokens = lex("value++ --value ? :").expect("valid tokens");
        assert_eq!(tokens[0].kind, TokenKind::Identifier("value".into()));
        assert_eq!(tokens[1].kind, TokenKind::PlusPlus);
        assert_eq!(tokens[1].span, Span::new(5, 7));
        assert_eq!(tokens[2].kind, TokenKind::MinusMinus);
        assert_eq!(tokens[3].kind, TokenKind::Identifier("value".into()));
        assert_eq!(tokens[4].kind, TokenKind::Question);
        assert_eq!(tokens[5].kind, TokenKind::Colon);
    }

    #[test]
    fn lexes_unsigned_and_decimal_suffixes() {
        let tokens = lex("10u 10U 10ul 10Lu 10L 2f 2.5d 1.5m 3m").expect("valid tokens");
        assert_eq!(tokens[0].kind, TokenKind::UIntLiteral("10".into()));
        assert_eq!(tokens[1].kind, TokenKind::UIntLiteral("10".into()));
        assert_eq!(tokens[2].kind, TokenKind::ULongLiteral("10".into()));
        assert_eq!(tokens[3].kind, TokenKind::ULongLiteral("10".into()));
        assert_eq!(tokens[4].kind, TokenKind::LongLiteral("10".into()));
        assert_eq!(tokens[5].kind, TokenKind::FloatLiteral("2".into()));
        assert_eq!(tokens[6].kind, TokenKind::DoubleLiteral("2.5".into()));
        assert_eq!(tokens[7].kind, TokenKind::DecimalLiteral("1.5".into()));
        assert_eq!(tokens[8].kind, TokenKind::DecimalLiteral("3".into()));
    }

    #[test]
    fn rejects_integer_suffixes_on_fractional_literals() {
        let diagnostics = lex("2.5L").expect_err("L is integer-only");
        assert!(diagnostics[0].message.contains("not valid on literals"));
        let diagnostics = lex("2.5u").expect_err("u is integer-only");
        assert!(diagnostics[0].message.contains("not valid on literals"));
    }

    #[test]
    fn lexes_new_type_keywords() {
        let tokens = lex("sbyte byte short ushort uint ulong decimal").expect("valid");
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();
        assert_eq!(
            kinds[..7],
            [
                TokenKind::SByte,
                TokenKind::Byte,
                TokenKind::Short,
                TokenKind::UShort,
                TokenKind::UInt,
                TokenKind::ULong,
                TokenKind::Decimal,
            ]
        );
    }

    #[test]
    fn former_ecs_words_lex_as_plain_identifiers() {
        let tokens = lex("component system read write").expect("valid source");
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.kind.clone())
                .collect::<Vec<_>>(),
            vec![
                TokenKind::Identifier("component".into()),
                TokenKind::Identifier("system".into()),
                TokenKind::Identifier("read".into()),
                TokenKind::Identifier("write".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn reports_unknown_characters() {
        let diagnostics = lex("value @").expect_err("@ is not valid syntax");
        assert_eq!(diagnostics[0].span.start, 6);
    }
}
