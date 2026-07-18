use std::mem::discriminant;

use aster_diagnostics::{Diagnostic, Span};

use crate::{
    Accessor, AssignmentOperator, BinaryOperator, Block, EnumCase, EnumDeclaration, Expression,
    ExpressionKind, Field, FieldInitializer, FunctionDeclaration, IncrementOperator, Item, Literal,
    Member, Module, Parameter, Property, Statement, SwitchCase, Token, TokenKind, TypeDeclaration,
    TypeParameter, TypeRef, UnaryOperator, VariableDeclaration, VariableKind, Visibility,
};

/// Build an AST from a positioned token stream.
///
/// # Errors
///
/// Returns diagnostics when the token stream does not conform to the implemented grammar.
pub fn parse(tokens: Vec<Token>) -> Result<Module, Vec<Diagnostic>> {
    let mut parser = Parser {
        tokens,
        cursor: 0,
        diagnostics: Vec::new(),
    };
    let module = parser.module();
    if parser.diagnostics.is_empty() {
        Ok(module)
    } else {
        Err(parser.diagnostics)
    }
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    diagnostics: Vec<Diagnostic>,
}

impl Parser {
    fn module(&mut self) -> Module {
        let namespace = if self.at(&TokenKind::Namespace) {
            self.namespace_declaration(&TokenKind::Namespace)
        } else if self.at(&TokenKind::Module) {
            self.diagnostics.push(
                Diagnostic::error("`module` was replaced by `namespace`", self.current().span)
                    .with_help("write `namespace name;` at the beginning of the file"),
            );
            self.namespace_declaration(&TokenKind::Module)
        } else {
            None
        };
        let mut usings = Vec::new();
        while self.at(&TokenKind::Using) || self.at(&TokenKind::Import) {
            let legacy = self.at(&TokenKind::Import);
            if legacy {
                self.diagnostics.push(
                    Diagnostic::error("`import` was replaced by `using`", self.current().span)
                        .with_help("write `using namespace.name;` before declarations"),
                );
            }
            let keyword = if legacy {
                TokenKind::Import
            } else {
                TokenKind::Using
            };
            if let Some(using) = self.using_declaration(&keyword) {
                usings.push(using);
            }
        }
        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            if self.at(&TokenKind::Module) {
                self.diagnostics.push(
                    Diagnostic::error("`module` was replaced by `namespace`", self.current().span)
                        .with_help(
                            "write one `namespace name;` before every using and declaration",
                        ),
                );
                self.synchronize_declaration();
                continue;
            }
            if self.at(&TokenKind::Import) {
                self.diagnostics.push(
                    Diagnostic::error("`import` was replaced by `using`", self.current().span)
                        .with_help("write `using namespace.name;` before declarations"),
                );
                self.synchronize_declaration();
                continue;
            }
            if self.at(&TokenKind::Namespace) || self.at(&TokenKind::Using) {
                self.error_here(
                    "namespace declarations and usings must appear before declarations",
                );
                self.synchronize_declaration();
                continue;
            }
            let cursor = self.cursor;
            if let Some(item) = self.item() {
                items.push(item);
            } else {
                if self.cursor == cursor {
                    self.error_here("expected a declaration");
                    self.advance();
                }
                self.synchronize_declaration();
            }
        }
        Module {
            namespace,
            usings,
            items,
        }
    }

    fn namespace_declaration(
        &mut self,
        keyword: &TokenKind,
    ) -> Option<crate::NamespaceDeclaration> {
        let start = self.expect(keyword)?.span.start;
        let name = self.module_name()?;
        let end = self.expect(&TokenKind::Semicolon)?.span.end;
        Some(crate::NamespaceDeclaration {
            name,
            span: Span::new(start, end),
        })
    }

    fn using_declaration(&mut self, keyword: &TokenKind) -> Option<crate::UsingDeclaration> {
        let start = self.expect(keyword)?.span.start;
        let name = self.module_name()?;
        let end = self.expect(&TokenKind::Semicolon)?.span.end;
        Some(crate::UsingDeclaration {
            name,
            span: Span::new(start, end),
        })
    }

    fn module_name(&mut self) -> Option<String> {
        let (first, _) = self.identifier()?;
        let mut name = first;
        while self.take(&TokenKind::Dot).is_some() {
            let (segment, _) = self.identifier()?;
            name.push('.');
            name.push_str(&segment);
        }
        Some(name)
    }

    fn item(&mut self) -> Option<Item> {
        let start = self.current().span.start;
        let visibility = self.visibility(Visibility::Internal);
        let is_static = self.take(&TokenKind::Static).is_some();
        if is_static
            && !matches!(
                self.current().kind,
                TokenKind::Class | TokenKind::Struct | TokenKind::Interface
            )
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "`static` at namespace level is valid only on a class",
                    self.current().span,
                )
                .with_help("write `static` before `class`, or remove the modifier"),
            );
        }
        match &self.current().kind {
            TokenKind::Class => self
                .type_declaration(visibility, start, TypeKind::Class, is_static)
                .map(Item::Class),
            TokenKind::Struct => self
                .type_declaration(visibility, start, TypeKind::Struct, is_static)
                .map(Item::Struct),
            TokenKind::Interface => self
                .type_declaration(visibility, start, TypeKind::Interface, is_static)
                .map(Item::Interface),
            TokenKind::Enum => self.enum_declaration(visibility, start).map(Item::Enum),
            TokenKind::Const | TokenKind::Var => {
                self.variable(Some(visibility)).map(Item::Variable)
            }
            _ if self.is_type_start() => self.typed_module_item(visibility, start),
            _ => None,
        }
    }

    fn type_declaration(
        &mut self,
        visibility: Visibility,
        start: usize,
        kind: TypeKind,
        is_static: bool,
    ) -> Option<TypeDeclaration> {
        self.advance();
        if is_static && kind != TypeKind::Class {
            self.diagnostics.push(
                Diagnostic::error("only classes can be static", self.current().span)
                    .with_help("remove `static` from this type declaration"),
            );
        }
        let (name, _) = self.identifier()?;
        let type_parameters = self.type_parameters()?;
        let interfaces = if let Some(colon) = self.take(&TokenKind::Colon) {
            if kind != TypeKind::Class {
                self.diagnostics.push(
                    Diagnostic::error(
                        "only classes can declare implemented interfaces",
                        colon.span,
                    )
                    .with_help("remove the interface list; interface implementation by structs is not supported yet"),
                );
            }
            let mut interfaces = Vec::new();
            loop {
                interfaces.push(self.type_ref()?);
                if self.take(&TokenKind::Comma).is_none() {
                    break;
                }
            }
            interfaces
        } else {
            Vec::new()
        };
        self.expect(&TokenKind::LeftBrace)?;
        let mut members = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            if let Some(member) = self.member(kind, &name) {
                members.push(member);
            } else {
                self.synchronize_member();
            }
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        Some(TypeDeclaration {
            visibility,
            is_static,
            name,
            type_parameters,
            interfaces,
            members,
            span: Span::new(start, end),
        })
    }

    fn enum_declaration(
        &mut self,
        visibility: Visibility,
        start: usize,
    ) -> Option<EnumDeclaration> {
        self.advance();
        let (name, _) = self.identifier()?;
        let type_parameters = self.type_parameters()?;
        self.expect(&TokenKind::LeftBrace)?;
        let mut cases = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let case_start = self.current().span.start;
            let (case_name, case_span) = self.identifier()?;
            let fields = if self.take(&TokenKind::LeftParen).is_some() {
                let fields = self.parameters()?;
                self.expect(&TokenKind::RightParen)?;
                fields
            } else {
                Vec::new()
            };
            let end = fields.last().map_or(case_span.end, |field| field.span.end);
            cases.push(EnumCase {
                name: case_name,
                fields,
                span: Span::new(case_start, end),
            });
            if self.take(&TokenKind::Comma).is_none() && !self.at(&TokenKind::RightBrace) {
                self.diagnostics.push(
                    Diagnostic::error("expected `,` between enum cases", self.current().span)
                        .with_help("separate enum cases with commas"),
                );
                return None;
            }
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        Some(EnumDeclaration {
            visibility,
            name,
            type_parameters,
            cases,
            span: Span::new(start, end),
        })
    }

    fn member(&mut self, owner: TypeKind, owner_name: &str) -> Option<Member> {
        let start = self.current().span.start;
        let default = if owner == TypeKind::Interface {
            Visibility::Public
        } else {
            Visibility::Private
        };
        let visibility = self.visibility(default);
        let is_static = self.take(&TokenKind::Static).is_some();
        if owner == TypeKind::Class
            && matches!(&self.current().kind, TokenKind::Identifier(name) if name == owner_name)
            && self.peek(1).kind == TokenKind::LeftParen
        {
            if is_static {
                self.diagnostics.push(
                    Diagnostic::error("constructors cannot be static", self.current().span)
                        .with_help("remove `static` from the constructor"),
                );
            }
            let token = self.advance().clone();
            return self
                .function_after_name(
                    visibility,
                    TypeRef::new("void", token.span),
                    owner_name.to_owned(),
                    start,
                    false,
                    false,
                )
                .map(|mut constructor| {
                    constructor.constructor = true;
                    Member::Method(constructor)
                });
        }
        let type_ref = self.type_ref()?;
        let (name, _) = self.identifier()?;
        if self.at(&TokenKind::LeftParen) || self.at(&TokenKind::Less) {
            self.function_after_name(
                visibility,
                type_ref,
                name,
                start,
                owner == TypeKind::Interface,
                is_static,
            )
            .map(Member::Method)
        } else if self.at(&TokenKind::LeftBrace) {
            if is_static {
                self.diagnostics.push(
                    Diagnostic::error("static properties are not implemented", self.current().span)
                        .with_help("remove `static` from the property"),
                );
            }
            self.property_after_name(visibility, type_ref, name, start)
                .map(Member::Property)
        } else {
            if is_static {
                self.diagnostics.push(
                    Diagnostic::error("static fields are not implemented", self.current().span)
                        .with_help("remove `static` from the field"),
                );
            }
            self.field_after_name(visibility, type_ref, name, start)
                .map(Member::Field)
        }
    }

    fn typed_module_item(&mut self, visibility: Visibility, start: usize) -> Option<Item> {
        let type_ref = self.type_ref()?;
        let (name, _) = self.identifier()?;
        if self.at(&TokenKind::LeftParen) || self.at(&TokenKind::Less) {
            self.function_after_name(visibility, type_ref, name, start, false, false)
                .map(Item::Function)
        } else {
            self.variable_after_name(
                Some(visibility),
                VariableKind::Explicit(type_ref),
                name,
                start,
            )
            .map(Item::Variable)
        }
    }

    fn function_after_name(
        &mut self,
        visibility: Visibility,
        return_type: TypeRef,
        name: String,
        start: usize,
        signature_only: bool,
        is_static: bool,
    ) -> Option<FunctionDeclaration> {
        let type_parameters = self.type_parameters()?;
        self.expect(&TokenKind::LeftParen)?;
        let parameters = self.parameters()?;
        let right_paren = self.expect(&TokenKind::RightParen)?;
        if signature_only {
            let end = self.expect(&TokenKind::Semicolon)?.span.end;
            Some(FunctionDeclaration {
                constructor: false,
                is_static,
                type_parameters,
                visibility,
                return_type,
                name,
                parameters,
                body: None,
                span: Span::new(start, end),
            })
        } else {
            let body = self.block()?;
            let end = body.span.end.max(right_paren.span.end);
            Some(FunctionDeclaration {
                constructor: false,
                is_static,
                type_parameters,
                visibility,
                return_type,
                name,
                parameters,
                body: Some(body),
                span: Span::new(start, end),
            })
        }
    }

    fn property_after_name(
        &mut self,
        visibility: Visibility,
        type_ref: TypeRef,
        name: String,
        start: usize,
    ) -> Option<Property> {
        self.expect(&TokenKind::LeftBrace)?;
        let mut getter = None;
        let mut setter = None;
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let accessor_start = self.current().span.start;
            let explicit_visibility = self.current_visibility().is_some();
            let accessor_visibility = self.visibility(visibility);
            let kind = self.advance().clone();
            let body = self.block()?;
            let accessor = Accessor {
                visibility: accessor_visibility,
                explicit_visibility,
                span: Span::new(accessor_start, body.span.end),
                body,
            };
            match kind.kind {
                TokenKind::Identifier(ref value) if value == "get" && getter.is_none() => {
                    getter = Some(accessor);
                }
                TokenKind::Identifier(ref value) if value == "set" && setter.is_none() => {
                    setter = Some(accessor);
                }
                TokenKind::Identifier(ref value) if value == "get" => self.diagnostics.push(
                    Diagnostic::error("a property can declare only one getter", kind.span),
                ),
                TokenKind::Identifier(ref value) if value == "set" => self.diagnostics.push(
                    Diagnostic::error("a property can declare only one setter", kind.span),
                ),
                _ => {
                    self.diagnostics.push(
                        Diagnostic::error("expected `get` or `set` accessor", kind.span)
                            .with_help("declare an explicit accessor body"),
                    );
                }
            }
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        Some(Property {
            visibility,
            type_ref,
            name,
            getter,
            setter,
            span: Span::new(start, end),
        })
    }

    fn type_parameters(&mut self) -> Option<Vec<TypeParameter>> {
        if self.take(&TokenKind::Less).is_none() {
            return Some(Vec::new());
        }
        let mut parameters = Vec::new();
        loop {
            let (name, span) = self.identifier()?;
            parameters.push(TypeParameter { name, span });
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(&TokenKind::Greater)?;
        Some(parameters)
    }

    fn parameters(&mut self) -> Option<Vec<Parameter>> {
        let mut parameters = Vec::new();
        if self.at(&TokenKind::RightParen) {
            return Some(parameters);
        }
        loop {
            let start = self.current().span.start;
            let type_ref = self.type_ref()?;
            let (name, name_span) = self.identifier()?;
            parameters.push(Parameter {
                type_ref,
                name,
                span: Span::new(start, name_span.end),
            });
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Some(parameters)
    }

    fn field_after_name(
        &mut self,
        visibility: Visibility,
        type_ref: TypeRef,
        name: String,
        start: usize,
    ) -> Option<Field> {
        let initializer = if self.take(&TokenKind::Equal).is_some() {
            Some(self.expression()?)
        } else {
            None
        };
        let end = self.expect(&TokenKind::Semicolon)?.span.end;
        Some(Field {
            visibility,
            type_ref,
            name,
            initializer,
            span: Span::new(start, end),
        })
    }

    fn variable(&mut self, visibility: Option<Visibility>) -> Option<VariableDeclaration> {
        let start = self.current().span.start;
        if self.take(&TokenKind::Const).is_some() {
            let type_ref = self.type_ref()?;
            let (name, _) = self.identifier()?;
            self.variable_after_name(visibility, VariableKind::Constant(type_ref), name, start)
        } else {
            self.expect(&TokenKind::Var)?;
            let (name, _) = self.identifier()?;
            self.variable_after_name(visibility, VariableKind::Inferred, name, start)
        }
    }

    fn variable_after_name(
        &mut self,
        visibility: Option<Visibility>,
        kind: VariableKind,
        name: String,
        start: usize,
    ) -> Option<VariableDeclaration> {
        let initializer = if self.take(&TokenKind::Equal).is_some() {
            Some(self.expression()?)
        } else {
            None
        };
        let end = self.expect(&TokenKind::Semicolon)?.span.end;
        Some(VariableDeclaration {
            visibility,
            kind,
            name,
            initializer,
            span: Span::new(start, end),
        })
    }

    fn block(&mut self) -> Option<Block> {
        let start = self.expect(&TokenKind::LeftBrace)?.span.start;
        let mut statements = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let cursor = self.cursor;
            if let Some(statement) = self.statement() {
                statements.push(statement);
            } else {
                if self.cursor == cursor {
                    self.advance();
                }
                self.synchronize_statement();
            }
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        Some(Block {
            statements,
            span: Span::new(start, end),
        })
    }

    fn statement(&mut self) -> Option<Statement> {
        if self.at(&TokenKind::If) {
            return self.if_statement();
        }
        if self.at(&TokenKind::While) {
            return self.while_statement();
        }
        if self.at(&TokenKind::For) {
            return self.for_statement();
        }
        if self.at(&TokenKind::Switch) {
            return self.switch_statement();
        }
        if self.at(&TokenKind::Break) || self.at(&TokenKind::Continue) {
            let token = self.advance().clone();
            let end = self.expect(&TokenKind::Semicolon)?.span.end;
            let span = Span::new(token.span.start, end);
            return Some(if token.kind == TokenKind::Break {
                Statement::Break(span)
            } else {
                Statement::Continue(span)
            });
        }
        if self.at(&TokenKind::Return) {
            return self.return_statement();
        }
        if self.at(&TokenKind::Const) || self.at(&TokenKind::Var) {
            return self.variable(None).map(Statement::Variable);
        }
        if self.is_typed_variable_start() {
            let start = self.current().span.start;
            let type_ref = self.type_ref()?;
            let (name, _) = self.identifier()?;
            return self
                .variable_after_name(None, VariableKind::Explicit(type_ref), name, start)
                .map(Statement::Variable);
        }
        let expression = self.expression()?;
        self.expect(&TokenKind::Semicolon)?;
        Some(Statement::Expression(expression))
    }

    fn if_statement(&mut self) -> Option<Statement> {
        let start = self.expect(&TokenKind::If)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let condition = self.expression()?;
        self.expect(&TokenKind::RightParen)?;
        let then_block = self.block()?;
        let else_block = if self.take(&TokenKind::Else).is_some() {
            if self.at(&TokenKind::If) {
                let nested = self.if_statement()?;
                let span = nested.span();
                Some(Block {
                    statements: vec![nested],
                    span,
                })
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        let end = else_block
            .as_ref()
            .map_or(then_block.span.end, |block| block.span.end);
        Some(Statement::If {
            condition,
            then_block,
            else_block,
            span: Span::new(start, end),
        })
    }

    fn while_statement(&mut self) -> Option<Statement> {
        let start = self.expect(&TokenKind::While)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let condition = self.expression()?;
        self.expect(&TokenKind::RightParen)?;
        let body = self.block()?;
        let end = body.span.end;
        Some(Statement::While {
            condition,
            body,
            span: Span::new(start, end),
        })
    }

    fn for_statement(&mut self) -> Option<Statement> {
        let start = self.expect(&TokenKind::For)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let initializer = if self.take(&TokenKind::Semicolon).is_some() {
            None
        } else if self.at(&TokenKind::Const) || self.at(&TokenKind::Var) {
            Some(Box::new(Statement::Variable(self.variable(None)?)))
        } else if self.is_typed_variable_start() {
            let variable_start = self.current().span.start;
            let type_ref = self.type_ref()?;
            let (name, _) = self.identifier()?;
            Some(Box::new(Statement::Variable(self.variable_after_name(
                None,
                VariableKind::Explicit(type_ref),
                name,
                variable_start,
            )?)))
        } else {
            let expression = self.expression()?;
            self.expect(&TokenKind::Semicolon)?;
            Some(Box::new(Statement::Expression(expression)))
        };
        let condition = if self.at(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(&TokenKind::Semicolon)?;
        let update = if self.at(&TokenKind::RightParen) {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(&TokenKind::RightParen)?;
        let body = self.block()?;
        let end = body.span.end;
        Some(Statement::For {
            initializer,
            condition,
            update,
            body,
            span: Span::new(start, end),
        })
    }

    fn return_statement(&mut self) -> Option<Statement> {
        let start = self.expect(&TokenKind::Return)?.span.start;
        let value = if self.at(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.expression()?)
        };
        let end = self.expect(&TokenKind::Semicolon)?.span.end;
        Some(Statement::Return {
            value,
            span: Span::new(start, end),
        })
    }

    fn switch_statement(&mut self) -> Option<Statement> {
        let start = self.expect(&TokenKind::Switch)?.span.start;
        self.expect(&TokenKind::LeftParen)?;
        let value = self.expression()?;
        self.expect(&TokenKind::RightParen)?;
        self.expect(&TokenKind::LeftBrace)?;
        let mut cases = Vec::new();
        let mut default = None;
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            if self.take(&TokenKind::Case).is_some() {
                let case_start = self.current().span.start;
                if default.is_some() {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "case after `default` is unreachable",
                            self.current().span,
                        )
                        .with_help("move `default` after every explicit case"),
                    );
                }
                let (first, first_span) = self.identifier()?;
                let (enum_name, case_name) = if self.take(&TokenKind::Dot).is_some() {
                    (Some(first), self.identifier()?.0)
                } else {
                    (None, first)
                };
                let mut bindings = Vec::new();
                if self.take(&TokenKind::LeftParen).is_some() {
                    if !self.at(&TokenKind::RightParen) {
                        loop {
                            bindings.push(self.identifier()?.0);
                            if self.take(&TokenKind::Comma).is_none() {
                                break;
                            }
                        }
                    }
                    self.expect(&TokenKind::RightParen)?;
                }
                self.expect(&TokenKind::Colon)?;
                let body = self.switch_case_body();
                cases.push(SwitchCase {
                    enum_name,
                    case_name,
                    bindings,
                    span: Span::new(case_start, body.span.end.max(first_span.end)),
                    body,
                });
            } else if self.at(&TokenKind::Default) {
                let default_span = self.advance().span;
                self.expect(&TokenKind::Colon)?;
                let body = self.switch_case_body();
                if default.replace(body).is_some() {
                    self.diagnostics.push(Diagnostic::error(
                        "a switch can declare only one default case",
                        default_span,
                    ));
                }
            } else {
                self.diagnostics.push(Diagnostic::error(
                    "expected `case` or `default` in switch",
                    self.current().span,
                ));
                return None;
            }
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        Some(Statement::Switch {
            value,
            cases,
            default,
            span: Span::new(start, end),
        })
    }

    fn switch_case_body(&mut self) -> Block {
        let start = self.current().span.start;
        let mut statements = Vec::new();
        while !self.at(&TokenKind::Case)
            && !self.at(&TokenKind::Default)
            && !self.at(&TokenKind::RightBrace)
            && !self.at(&TokenKind::Eof)
        {
            let cursor = self.cursor;
            if let Some(statement) = self.statement() {
                statements.push(statement);
            } else {
                if self.cursor == cursor {
                    self.advance();
                }
                self.synchronize_statement();
            }
        }
        let end = statements
            .last()
            .map_or(start, |statement| statement.span().end);
        Block {
            statements,
            span: Span::new(start, end),
        }
    }

    fn expression(&mut self) -> Option<Expression> {
        self.assignment_expression()
    }

    fn assignment_expression(&mut self) -> Option<Expression> {
        let target = self.conditional()?;
        let operator = match self.current().kind {
            TokenKind::Equal => Some(AssignmentOperator::Assign),
            TokenKind::PlusEqual => Some(AssignmentOperator::AddAssign),
            TokenKind::MinusEqual => Some(AssignmentOperator::SubtractAssign),
            TokenKind::StarEqual => Some(AssignmentOperator::MultiplyAssign),
            TokenKind::SlashEqual => Some(AssignmentOperator::DivideAssign),
            _ => None,
        };
        let Some(operator) = operator else {
            return Some(target);
        };
        self.advance();
        let value = self.assignment_expression()?;
        let span = Span::new(target.span.start, value.span.end);
        Some(Expression {
            kind: ExpressionKind::Assignment {
                target: Box::new(target),
                operator,
                value: Box::new(value),
            },
            span,
        })
    }

    /// `condition ? whenTrue : whenFalse`, right-associative, above assignment
    /// and below `||` in precedence.
    fn conditional(&mut self) -> Option<Expression> {
        let condition = self.logical_or()?;
        if self.take(&TokenKind::Question).is_none() {
            return Some(condition);
        }
        let when_true = self.expression()?;
        self.expect(&TokenKind::Colon)?;
        let when_false = self.assignment_expression()?;
        let span = Span::new(condition.span.start, when_false.span.end);
        Some(Expression {
            kind: ExpressionKind::Conditional {
                condition: Box::new(condition),
                when_true: Box::new(when_true),
                when_false: Box::new(when_false),
            },
            span,
        })
    }

    fn logical_or(&mut self) -> Option<Expression> {
        self.binary_level(
            Self::logical_and,
            &[(TokenKind::OrOr, BinaryOperator::LogicalOr)],
        )
    }

    fn logical_and(&mut self) -> Option<Expression> {
        self.binary_level(
            Self::equality,
            &[(TokenKind::AndAnd, BinaryOperator::LogicalAnd)],
        )
    }

    fn equality(&mut self) -> Option<Expression> {
        self.binary_level(
            Self::comparison,
            &[
                (TokenKind::EqualEqual, BinaryOperator::Equal),
                (TokenKind::BangEqual, BinaryOperator::NotEqual),
            ],
        )
    }

    fn comparison(&mut self) -> Option<Expression> {
        self.binary_level(
            Self::term,
            &[
                (TokenKind::Less, BinaryOperator::Less),
                (TokenKind::LessEqual, BinaryOperator::LessEqual),
                (TokenKind::Greater, BinaryOperator::Greater),
                (TokenKind::GreaterEqual, BinaryOperator::GreaterEqual),
            ],
        )
    }

    fn term(&mut self) -> Option<Expression> {
        self.binary_level(
            Self::factor,
            &[
                (TokenKind::Plus, BinaryOperator::Add),
                (TokenKind::Minus, BinaryOperator::Subtract),
            ],
        )
    }

    fn factor(&mut self) -> Option<Expression> {
        self.binary_level(
            Self::unary,
            &[
                (TokenKind::Star, BinaryOperator::Multiply),
                (TokenKind::Slash, BinaryOperator::Divide),
                (TokenKind::Percent, BinaryOperator::Remainder),
            ],
        )
    }

    fn binary_level(
        &mut self,
        operand: fn(&mut Self) -> Option<Expression>,
        operators: &[(TokenKind, BinaryOperator)],
    ) -> Option<Expression> {
        let mut expression = operand(self)?;
        while let Some((_, operator)) = operators.iter().find(|(token, _)| self.at(token)) {
            let operator = *operator;
            self.advance();
            let right = operand(self)?;
            let span = Span::new(expression.span.start, right.span.end);
            expression = Expression {
                kind: ExpressionKind::Binary {
                    left: Box::new(expression),
                    operator,
                    right: Box::new(right),
                },
                span,
            };
        }
        Some(expression)
    }

    fn unary(&mut self) -> Option<Expression> {
        if let Some(target) = self.cast_target() {
            let start = self.advance().span.start;
            let type_token = self.advance().clone();
            self.expect(&TokenKind::RightParen)?;
            let target = TypeRef::new(target, type_token.span);
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Some(Expression {
                kind: ExpressionKind::Cast {
                    target,
                    operand: Box::new(operand),
                },
                span,
            });
        }
        if let Some(operator) = self.increment_operator() {
            let start = self.advance().span.start;
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Some(Expression {
                kind: ExpressionKind::IncrementDecrement {
                    operator,
                    prefix: true,
                    operand: Box::new(operand),
                },
                span,
            });
        }
        let operator = match self.current().kind {
            TokenKind::Bang => Some(UnaryOperator::Not),
            TokenKind::Minus => Some(UnaryOperator::Negate),
            _ => None,
        };
        if let Some(operator) = operator {
            let start = self.advance().span.start;
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Some(Expression {
                kind: ExpressionKind::Unary {
                    operator,
                    operand: Box::new(operand),
                },
                span,
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Option<Expression> {
        let mut expression = self.primary()?;
        loop {
            if self.take(&TokenKind::Dot).is_some() {
                let (name, name_span) = self.identifier()?;
                let span = Span::new(expression.span.start, name_span.end);
                expression = Expression {
                    kind: ExpressionKind::Member {
                        object: Box::new(expression),
                        name,
                    },
                    span,
                };
            } else if self.take(&TokenKind::LeftBracket).is_some() {
                let start = expression.span.start;
                let index = self.expression()?;
                let end = self.expect(&TokenKind::RightBracket)?.span.end;
                expression = Expression {
                    kind: ExpressionKind::Index {
                        array: Box::new(expression),
                        index: Box::new(index),
                    },
                    span: Span::new(start, end),
                };
            } else if self.at(&TokenKind::Less) && self.generic_call_ahead() {
                let type_arguments = self.type_arguments()?;
                self.expect(&TokenKind::LeftParen)?;
                expression = self.call_after_arguments(expression, type_arguments)?;
            } else if self.take(&TokenKind::LeftParen).is_some() {
                expression = self.call_after_arguments(expression, Vec::new())?;
            } else if let Some(operator) = self.increment_operator() {
                let end = self.advance().span.end;
                let span = Span::new(expression.span.start, end);
                expression = Expression {
                    kind: ExpressionKind::IncrementDecrement {
                        operator,
                        prefix: false,
                        operand: Box::new(expression),
                    },
                    span,
                };
            } else if self.at_try_propagation() {
                let end = self.advance().span.end;
                let span = Span::new(expression.span.start, end);
                expression = Expression {
                    kind: ExpressionKind::Try {
                        operand: Box::new(expression),
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Some(expression)
    }

    fn call_after_arguments(
        &mut self,
        mut expression: Expression,
        type_arguments: Vec<TypeRef>,
    ) -> Option<Expression> {
        let mut arguments = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                arguments.push(self.expression()?);
                if self.take(&TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        let end = self.expect(&TokenKind::RightParen)?.span.end;
        let span = Span::new(expression.span.start, end);
        expression = Expression {
            kind: ExpressionKind::Call {
                callee: Box::new(expression),
                type_arguments,
                arguments,
            },
            span,
        };
        Some(expression)
    }

    fn type_arguments(&mut self) -> Option<Vec<TypeRef>> {
        self.expect(&TokenKind::Less)?;
        let mut arguments = Vec::new();
        loop {
            arguments.push(self.type_ref()?);
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(&TokenKind::Greater)?;
        Some(arguments)
    }

    fn generic_call_ahead(&self) -> bool {
        self.generic_arguments_end(0)
            .is_some_and(|offset| self.peek(offset).kind == TokenKind::LeftParen)
    }

    /// A trailing `?` is the postfix `Result` propagation operator only when the
    /// following token cannot begin a ternary consequent. That keeps `expr?`
    /// distinct from `condition ? a : b` without lookahead beyond one token, and
    /// leaves `?.`/`??` to fail as controlled syntax errors for now.
    fn at_try_propagation(&self) -> bool {
        self.at(&TokenKind::Question)
            && matches!(
                self.peek(1).kind,
                TokenKind::Semicolon
                    | TokenKind::Comma
                    | TokenKind::RightParen
                    | TokenKind::RightBracket
                    | TokenKind::RightBrace
                    | TokenKind::Colon
                    | TokenKind::Eof
                    | TokenKind::Plus
                    | TokenKind::Star
                    | TokenKind::Slash
                    | TokenKind::Percent
                    | TokenKind::EqualEqual
                    | TokenKind::BangEqual
                    | TokenKind::Less
                    | TokenKind::LessEqual
                    | TokenKind::Greater
                    | TokenKind::GreaterEqual
                    | TokenKind::AndAnd
                    | TokenKind::OrOr
                    | TokenKind::Equal
                    | TokenKind::PlusEqual
                    | TokenKind::MinusEqual
                    | TokenKind::StarEqual
                    | TokenKind::SlashEqual
            )
    }

    /// Detect `(type)` for the builtin value types. Type keywords are never
    /// expressions, so this is unambiguous with parenthesized expressions.
    fn cast_target(&self) -> Option<&'static str> {
        if !self.at(&TokenKind::LeftParen) || self.peek(2).kind != TokenKind::RightParen {
            return None;
        }
        match self.peek(1).kind {
            TokenKind::SByte => Some("sbyte"),
            TokenKind::Byte => Some("byte"),
            TokenKind::Short => Some("short"),
            TokenKind::UShort => Some("ushort"),
            TokenKind::Int => Some("int"),
            TokenKind::UInt => Some("uint"),
            TokenKind::Long => Some("long"),
            TokenKind::ULong => Some("ulong"),
            TokenKind::Float => Some("float"),
            TokenKind::Double => Some("double"),
            TokenKind::Decimal => Some("decimal"),
            TokenKind::Char => Some("char"),
            _ => None,
        }
    }

    fn increment_operator(&self) -> Option<IncrementOperator> {
        match self.current().kind {
            TokenKind::PlusPlus => Some(IncrementOperator::Increment),
            TokenKind::MinusMinus => Some(IncrementOperator::Decrement),
            _ => None,
        }
    }

    fn primary(&mut self) -> Option<Expression> {
        let token = self.advance().clone();
        let kind = match token.kind {
            TokenKind::IntegerLiteral(value) => ExpressionKind::Literal(Literal::Integer(value)),
            TokenKind::LongLiteral(value) => ExpressionKind::Literal(Literal::Long(value)),
            TokenKind::UIntLiteral(value) => ExpressionKind::Literal(Literal::UInt(value)),
            TokenKind::ULongLiteral(value) => ExpressionKind::Literal(Literal::ULong(value)),
            TokenKind::FloatLiteral(value) => ExpressionKind::Literal(Literal::Float(value)),
            TokenKind::DoubleLiteral(value) => ExpressionKind::Literal(Literal::Double(value)),
            TokenKind::DecimalLiteral(value) => ExpressionKind::Literal(Literal::Decimal(value)),
            TokenKind::StringLiteral(value) => ExpressionKind::Literal(Literal::String(value)),
            TokenKind::CharacterLiteral(value) => {
                ExpressionKind::Literal(Literal::Character(value))
            }
            TokenKind::True => ExpressionKind::Literal(Literal::Boolean(true)),
            TokenKind::False => ExpressionKind::Literal(Literal::Boolean(false)),
            TokenKind::LeftBracket => return self.array_literal(token.span.start),
            TokenKind::New => return self.new_array(token.span.start),
            TokenKind::This => ExpressionKind::This,
            TokenKind::Identifier(name) if self.at(&TokenKind::LeftBrace) => {
                return self.struct_literal(name, token.span.start);
            }
            TokenKind::Identifier(mut name) if self.generic_struct_literal_ahead() => {
                let arguments = self.type_arguments()?;
                name.push('<');
                name.push_str(
                    &arguments
                        .iter()
                        .map(|argument| argument.name.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                );
                name.push('>');
                return self.struct_literal(name, token.span.start);
            }
            TokenKind::Identifier(mut name) if self.generic_member_ahead() => {
                let arguments = self.type_arguments()?;
                name.push('<');
                name.push_str(
                    &arguments
                        .iter()
                        .map(|argument| argument.name.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                );
                name.push('>');
                ExpressionKind::Name(name)
            }
            TokenKind::Identifier(name) => ExpressionKind::Name(name),
            TokenKind::LeftParen => {
                let expression = self.expression()?;
                let end = self.expect(&TokenKind::RightParen)?.span.end;
                return Some(Expression {
                    span: Span::new(token.span.start, end),
                    ..expression
                });
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("expected expression, found {}", token.kind.description()),
                        token.span,
                    )
                    .with_help("start an expression with a literal, name, unary operator, or `(`"),
                );
                return None;
            }
        };
        Some(Expression {
            kind,
            span: token.span,
        })
    }

    fn struct_literal(&mut self, type_name: String, start: usize) -> Option<Expression> {
        self.expect(&TokenKind::LeftBrace)?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let (name, name_span) = self.identifier()?;
            self.expect(&TokenKind::Colon)?;
            let value = self.expression()?;
            let end = value.span.end;
            fields.push(FieldInitializer {
                name,
                value,
                span: Span::new(name_span.start, end),
            });
            if self.at(&TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        let end = self.expect(&TokenKind::RightBrace)?.span.end;
        Some(Expression {
            kind: ExpressionKind::StructLiteral { type_name, fields },
            span: Span::new(start, end),
        })
    }

    fn array_literal(&mut self, start: usize) -> Option<Expression> {
        let mut elements = Vec::new();
        if !self.at(&TokenKind::RightBracket) {
            loop {
                elements.push(self.expression()?);
                if self.take(&TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        let end = self.expect(&TokenKind::RightBracket)?.span.end;
        Some(Expression {
            kind: ExpressionKind::ArrayLiteral(elements),
            span: Span::new(start, end),
        })
    }

    fn new_array(&mut self, start: usize) -> Option<Expression> {
        let element_type = self.type_ref_base()?;
        if self.take(&TokenKind::LeftBracket).is_some() {
            let length = self.expression()?;
            let end = self.expect(&TokenKind::RightBracket)?.span.end;
            return Some(Expression {
                kind: ExpressionKind::NewArray {
                    element_type,
                    length: Box::new(length),
                },
                span: Span::new(start, end),
            });
        }
        self.expect(&TokenKind::LeftParen)?;
        let mut arguments = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                arguments.push(self.expression()?);
                if self.take(&TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        let end = self.expect(&TokenKind::RightParen)?.span.end;
        Some(Expression {
            kind: ExpressionKind::NewObject {
                type_name: element_type.name,
                arguments,
            },
            span: Span::new(start, end),
        })
    }

    fn generic_struct_literal_ahead(&self) -> bool {
        self.generic_arguments_end(0)
            .is_some_and(|offset| self.peek(offset).kind == TokenKind::LeftBrace)
    }

    fn generic_member_ahead(&self) -> bool {
        self.generic_arguments_end(0)
            .is_some_and(|offset| self.peek(offset).kind == TokenKind::Dot)
    }

    fn visibility(&mut self, default: Visibility) -> Visibility {
        let mut visibility = None;
        while let Some(current) = self.current_visibility() {
            let span = self.advance().span;
            if visibility.is_some() {
                self.diagnostics.push(
                    Diagnostic::error("only one visibility modifier is allowed", span)
                        .with_help("remove the extra visibility modifier"),
                );
            } else {
                visibility = Some(current);
            }
        }
        visibility.unwrap_or(default)
    }

    fn current_visibility(&self) -> Option<Visibility> {
        match self.current().kind {
            TokenKind::Public => Some(Visibility::Public),
            TokenKind::Internal => Some(Visibility::Internal),
            TokenKind::Protected => Some(Visibility::Protected),
            TokenKind::Private => Some(Visibility::Private),
            _ => None,
        }
    }

    fn type_ref(&mut self) -> Option<TypeRef> {
        let mut type_ref = self.type_ref_base()?;
        if self.take(&TokenKind::LeftBracket).is_some() {
            let end = self.expect(&TokenKind::RightBracket)?.span.end;
            type_ref.name.push_str("[]");
            type_ref.span.end = end;
        }
        Some(type_ref)
    }

    fn type_ref_base(&mut self) -> Option<TypeRef> {
        let token = self.advance().clone();
        let name = match token.kind {
            TokenKind::Void => "void".to_owned(),
            TokenKind::Bool => "bool".to_owned(),
            TokenKind::SByte => "sbyte".to_owned(),
            TokenKind::Byte => "byte".to_owned(),
            TokenKind::Short => "short".to_owned(),
            TokenKind::UShort => "ushort".to_owned(),
            TokenKind::Int => "int".to_owned(),
            TokenKind::UInt => "uint".to_owned(),
            TokenKind::Long => "long".to_owned(),
            TokenKind::ULong => "ulong".to_owned(),
            TokenKind::Float => "float".to_owned(),
            TokenKind::Double => "double".to_owned(),
            TokenKind::Decimal => "decimal".to_owned(),
            TokenKind::Char => "char".to_owned(),
            TokenKind::String => "string".to_owned(),
            TokenKind::Identifier(name) => name,
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    format!("expected type, found {}", token.kind.description()),
                    token.span,
                ));
                return None;
            }
        };
        let mut type_ref = TypeRef::new(name, token.span);
        if self.at(&TokenKind::Less) {
            let arguments = self.type_arguments()?;
            type_ref.name.push('<');
            type_ref.name.push_str(
                &arguments
                    .iter()
                    .map(|argument| argument.name.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            type_ref.name.push('>');
            type_ref.span.end = self.peek(0).span.start;
        }
        Some(type_ref)
    }

    fn is_type_start(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::Void | TokenKind::Identifier(_)
        ) || self.is_value_type_keyword()
    }

    fn is_typed_variable_start(&self) -> bool {
        match self.current().kind {
            TokenKind::Identifier(_) => self
                .type_ref_end(0)
                .is_some_and(|offset| matches!(self.peek(offset).kind, TokenKind::Identifier(_))),
            _ => self.is_value_type_keyword(),
        }
    }

    fn type_ref_end(&self, start: usize) -> Option<usize> {
        let mut offset = start + 1;
        if self.peek(offset).kind == TokenKind::Less {
            offset = self.generic_arguments_end(offset)?;
        }
        if self.peek(offset).kind == TokenKind::LeftBracket
            && self.peek(offset + 1).kind == TokenKind::RightBracket
        {
            offset += 2;
        }
        Some(offset)
    }

    /// Returns the offset immediately after a balanced generic argument list.
    fn generic_arguments_end(&self, start: usize) -> Option<usize> {
        if self.peek(start).kind != TokenKind::Less {
            return None;
        }
        let mut depth = 0usize;
        let mut offset = start;
        loop {
            match self.peek(offset).kind {
                TokenKind::Less => depth += 1,
                TokenKind::Greater => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(offset + 1);
                    }
                }
                TokenKind::Eof => return None,
                _ => {}
            }
            offset += 1;
        }
    }

    fn is_value_type_keyword(&self) -> bool {
        matches!(
            self.current().kind,
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

    fn identifier(&mut self) -> Option<(String, Span)> {
        let token = self.advance().clone();
        if let TokenKind::Identifier(name) = token.kind {
            Some((name, token.span))
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("expected identifier, found {}", token.kind.description()),
                    token.span,
                )
                .with_help("provide a declaration or binding name"),
            );
            None
        }
    }

    fn expect(&mut self, expected: &TokenKind) -> Option<Token> {
        if self.at(expected) {
            Some(self.advance().clone())
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "expected {}, found {}",
                        expected.description(),
                        self.current().kind.description()
                    ),
                    self.current().span,
                )
                .with_help(format!("insert {} here", expected.description())),
            );
            None
        }
    }

    fn take(&mut self, expected: &TokenKind) -> Option<Token> {
        self.at(expected).then(|| self.advance().clone())
    }

    fn at(&self, expected: &TokenKind) -> bool {
        discriminant(&self.current().kind) == discriminant(expected)
    }

    fn current(&self) -> &Token {
        self.peek(0)
    }

    fn peek(&self, offset: usize) -> &Token {
        &self.tokens[(self.cursor + offset).min(self.tokens.len() - 1)]
    }

    fn advance(&mut self) -> &Token {
        let index = self.cursor;
        if !self.at(&TokenKind::Eof) {
            self.cursor += 1;
        }
        &self.tokens[index]
    }

    fn error_here(&mut self, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::error(message, self.current().span));
    }

    /// Skip to the next statement boundary after a statement-level parse error,
    /// so one bad statement does not invalidate the rest of the function.
    fn synchronize_statement(&mut self) {
        loop {
            match self.current().kind {
                TokenKind::Semicolon => {
                    self.advance();
                    return;
                }
                TokenKind::RightBrace
                | TokenKind::Eof
                | TokenKind::If
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Return
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::Const
                | TokenKind::Var => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// Skip a malformed type member, including any balanced `{}` body, and stop
    /// before the closing `}` of the declaring type.
    fn synchronize_member(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.current().kind {
                TokenKind::Eof => return,
                TokenKind::LeftBrace => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::RightBrace => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.advance();
                    if depth == 0 {
                        return;
                    }
                }
                TokenKind::Semicolon if depth == 0 => {
                    self.advance();
                    return;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// Skip to the next namespace-level declaration boundary after a failed item.
    fn synchronize_declaration(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.current().kind {
                TokenKind::Eof => return,
                TokenKind::LeftBrace => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::RightBrace => {
                    self.advance();
                    if depth <= 1 {
                        return;
                    }
                    depth -= 1;
                }
                TokenKind::Semicolon if depth == 0 => {
                    self.advance();
                    return;
                }
                TokenKind::Public
                | TokenKind::Internal
                | TokenKind::Protected
                | TokenKind::Private
                | TokenKind::Class
                | TokenKind::Struct
                | TokenKind::Interface
                | TokenKind::Enum
                | TokenKind::Const
                | TokenKind::Var
                    if depth == 0 =>
                {
                    return;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeKind {
    Class,
    Struct,
    Interface,
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{
        BinaryOperator, ExpressionKind, IncrementOperator, Item, Member, Statement, Visibility, lex,
    };

    fn parse_source(source: &str) -> crate::Module {
        parse(lex(source).expect("lexing succeeds")).expect("parsing succeeds")
    }

    fn first_function_statements(module: &crate::Module) -> &[Statement] {
        let Item::Function(function) = &module.items[0] else {
            panic!("first item should be a function");
        };
        &function.body.as_ref().unwrap().statements
    }

    #[test]
    fn parses_class_struct_interface_and_function() {
        let source = r"
            public class Calculator { public int Add(int a, int b) { return a + b; } }
            public struct Position { public float x; public float y; }
            public interface IDamageable { void Damage(int amount); bool IsAlive(); }
            public int Add(int a, int b) { return a + b; }
        ";
        let module = parse_source(source);
        assert!(matches!(module.items[0], Item::Class(_)));
        assert!(matches!(module.items[1], Item::Struct(_)));
        assert!(matches!(module.items[2], Item::Interface(_)));
        assert!(matches!(module.items[3], Item::Function(_)));
        let Item::Class(class) = &module.items[0] else {
            unreachable!();
        };
        assert!(matches!(class.members[0], Member::Method(_)));
    }

    #[test]
    fn parses_namespace_and_usings_before_items() {
        let module = parse_source(
            "namespace app.main; using aster.math; using app.player; public int Run() { return 0; }",
        );
        assert_eq!(module.namespace.as_ref().unwrap().name, "app.main");
        assert_eq!(module.usings.len(), 2);
        assert_eq!(module.usings[0].name, "aster.math");
        assert_eq!(module.usings[1].name, "app.player");
    }

    #[test]
    fn rejects_using_after_a_declaration() {
        let source = "namespace app; public int Run() { return 0; } using aster.math;";
        let diagnostics = parse(lex(source).unwrap()).expect_err("late usings are invalid");
        assert!(diagnostics[0].message.contains("before declarations"));
    }

    #[test]
    fn rejects_legacy_module_and_import_with_migration_help() {
        for (source, expected) in [
            (
                "module app; public int Run() { return 0; }",
                "`module` was replaced by `namespace`",
            ),
            (
                "import app; public int Run() { return 0; }",
                "`import` was replaced by `using`",
            ),
        ] {
            let diagnostics = parse(lex(source).unwrap()).expect_err("legacy syntax is rejected");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message == expected)
            );
        }
    }

    #[test]
    fn applies_default_visibility() {
        let module = parse_source("class Sample { int value; void Reset() {} }");
        let Item::Class(class) = &module.items[0] else {
            unreachable!();
        };
        assert_eq!(class.visibility, Visibility::Internal);
        let Member::Field(field) = &class.members[0] else {
            unreachable!();
        };
        assert_eq!(field.visibility, Visibility::Private);
    }

    #[test]
    fn honors_operator_precedence_and_right_associative_assignment() {
        let module =
            parse_source("void Test() { int a = 0; int b = 0; int c = 0; a = b = 1 + 2 * 3; }");
        let Item::Function(function) = &module.items[0] else {
            unreachable!();
        };
        let Statement::Expression(expression) = &function.body.as_ref().unwrap().statements[3]
        else {
            unreachable!();
        };
        let ExpressionKind::Assignment { value, .. } = &expression.kind else {
            unreachable!();
        };
        let ExpressionKind::Assignment { value, .. } = &value.kind else {
            unreachable!();
        };
        let ExpressionKind::Binary {
            operator, right, ..
        } = &value.kind
        else {
            unreachable!();
        };
        assert_eq!(*operator, BinaryOperator::Add);
        assert!(matches!(
            right.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Multiply,
                ..
            }
        ));
    }

    #[test]
    fn parses_prefix_and_postfix_increment_decrement() {
        let module = parse_source("void Test() { int i = 0; i++; ++i; i--; --i; }");
        let statements = first_function_statements(&module);
        let expected = [
            (1usize, false, IncrementOperator::Increment),
            (2, true, IncrementOperator::Increment),
            (3, false, IncrementOperator::Decrement),
            (4, true, IncrementOperator::Decrement),
        ];
        for (index, expected_prefix, expected_operator) in expected {
            let Statement::Expression(expression) = &statements[index] else {
                panic!("expected expression statement");
            };
            let ExpressionKind::IncrementDecrement {
                operator, prefix, ..
            } = &expression.kind
            else {
                panic!("expected increment/decrement expression");
            };
            assert_eq!(*prefix, expected_prefix);
            assert_eq!(*operator, expected_operator);
        }
    }

    #[test]
    fn parses_increment_in_for_update() {
        let module = parse_source("void Test() { for (int i = 0; i < 3; i++) { } }");
        let statements = first_function_statements(&module);
        let Statement::For { update, .. } = &statements[0] else {
            panic!("expected for statement");
        };
        assert!(matches!(
            update.as_ref().unwrap().kind,
            ExpressionKind::IncrementDecrement { prefix: false, .. }
        ));
    }

    #[test]
    fn parses_right_associative_conditional() {
        let module = parse_source("int Test(bool a, bool b) { return a ? 1 : b ? 2 : 3; }");
        let statements = first_function_statements(&module);
        let Statement::Return { value, .. } = &statements[0] else {
            unreachable!();
        };
        let ExpressionKind::Conditional { when_false, .. } = &value.as_ref().unwrap().kind else {
            panic!("expected conditional expression");
        };
        assert!(matches!(
            when_false.kind,
            ExpressionKind::Conditional { .. }
        ));
    }

    #[test]
    fn conditional_binds_below_logical_or() {
        let module = parse_source("int Test(bool a, bool b) { return a || b ? 1 : 2; }");
        let statements = first_function_statements(&module);
        let Statement::Return { value, .. } = &statements[0] else {
            unreachable!();
        };
        let ExpressionKind::Conditional { condition, .. } = &value.as_ref().unwrap().kind else {
            panic!("expected conditional expression");
        };
        assert!(matches!(
            condition.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::LogicalOr,
                ..
            }
        ));
    }

    #[test]
    fn recovers_from_statement_error_and_reports_later_errors() {
        let source = "void Test() { int a = ; a = 1; b = 2; int c = 3; }";
        let diagnostics = parse(lex(source).expect("lexing succeeds"))
            .expect_err("missing initializer expression is an error");
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.message.starts_with("expected expression"))
                .count(),
            1
        );
        assert!(diagnostics.len() <= 2);
    }

    #[test]
    fn recovery_does_not_cascade_across_functions() {
        let source = "void Broken() { int a = ; } public int Fine() { return 1; }";
        let diagnostics =
            parse(lex(source).expect("lexing succeeds")).expect_err("first function is broken");
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn former_ecs_words_are_ordinary_identifiers_in_declarations() {
        // `component`, `system`, `read`, and `write` no longer name anything
        // special; they parse as ordinary identifiers wherever one is expected.
        let source = r"
            public struct component { public int system; }
            public int read(int write) { return write; }
        ";
        let module = parse_source(source);
        let Item::Struct(declaration) = &module.items[0] else {
            panic!("expected a struct named `component`")
        };
        assert_eq!(declaration.name, "component");
        let Item::Function(function) = &module.items[1] else {
            panic!("expected a function named `read`")
        };
        assert_eq!(function.name, "read");
        assert_eq!(function.parameters[0].name, "write");
    }

    #[test]
    fn old_component_declaration_is_no_longer_a_recognized_item() {
        // `component Position { float x; }` used to be a dedicated ECS item.
        // `component` is now a plain identifier, so this is parsed as an
        // ordinary (invalid) declaration, not silently accepted as ECS.
        let tokens = lex("component Position { float x; }").expect("lexing succeeds");
        assert!(parse(tokens).is_err());
    }

    #[test]
    fn old_system_declaration_parses_as_an_ordinary_function_not_ecs() {
        // The old ECS shape `system Name(Component access) { ... }` happens to be
        // syntactically valid under general function-declaration rules once
        // `system`/`read`/`write` are plain identifiers: `system` becomes the
        // return type, `Move` the function name, and `Position read` an
        // ordinary `(type, name)` parameter. It is no longer an `Item::System`.
        let module = parse_source("system Move(Position read) {}");
        let Item::Function(function) = &module.items[0] else {
            panic!("expected an ordinary function declaration")
        };
        assert_eq!(function.return_type.name, "system");
        assert_eq!(function.name, "Move");
        assert_eq!(function.parameters[0].type_ref.name, "Position");
        assert_eq!(function.parameters[0].name, "read");
    }

    #[test]
    fn old_foreach_statement_is_no_longer_accepted() {
        let source = "public void F() { foreach (item) { } }";
        let tokens = lex(source).expect("lexing succeeds");
        assert!(parse(tokens).is_err());
    }
}
