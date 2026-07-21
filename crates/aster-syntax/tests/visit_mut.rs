use aster_diagnostics::Span;
use aster_syntax::{
    Accessor, AssignmentOperator, BinaryOperator, Block, EnumCase, EnumDeclaration, Expression,
    ExpressionKind, Field, FieldInitializer, FunctionDeclaration, IncrementOperator, Item, Literal,
    Member, Module, NamespaceDeclaration, Parameter, Property, Statement, SwitchCase,
    TypeDeclaration, TypeParameter, TypeRef, UnaryOperator, UsingDeclaration, VariableDeclaration,
    VariableKind, Visibility,
    visit::{
        AstVisitorMut, walk_enum_declaration_mut, walk_expression_mut,
        walk_function_declaration_mut, walk_item_mut, walk_switch_case_mut,
        walk_type_declaration_mut,
    },
};

fn span(value: usize) -> Span {
    Span::new(value, value + 1)
}

fn type_ref(name: &str, position: usize) -> TypeRef {
    TypeRef::new(name, span(position))
}

fn literal(position: usize) -> Expression {
    Expression {
        kind: ExpressionKind::Literal(Literal::Integer(position.to_string())),
        span: span(position),
    }
}

fn name(value: &str, position: usize) -> Expression {
    Expression {
        kind: ExpressionKind::Name(value.to_owned()),
        span: span(position),
    }
}

fn expression_tree() -> Expression {
    Expression {
        kind: ExpressionKind::Assignment {
            target: Box::new(Expression {
                kind: ExpressionKind::Index {
                    array: Box::new(Expression {
                        kind: ExpressionKind::Member {
                            object: Box::new(Expression {
                                kind: ExpressionKind::NewObject {
                                    type_name: "Object<T>".to_owned(),
                                    arguments: vec![Expression {
                                        kind: ExpressionKind::StructLiteral {
                                            type_name: "Value<U>".to_owned(),
                                            fields: vec![FieldInitializer {
                                                name: "payload".to_owned(),
                                                value: literal(101),
                                                span: span(102),
                                            }],
                                        },
                                        span: span(103),
                                    }],
                                },
                                span: span(104),
                            }),
                            name: "items".to_owned(),
                        },
                        span: span(105),
                    }),
                    index: Box::new(Expression {
                        kind: ExpressionKind::Unary {
                            operator: UnaryOperator::Negate,
                            operand: Box::new(literal(106)),
                        },
                        span: span(107),
                    }),
                },
                span: span(108),
            }),
            operator: AssignmentOperator::Assign,
            value: Box::new(Expression {
                kind: ExpressionKind::Conditional {
                    condition: Box::new(Expression {
                        kind: ExpressionKind::Binary {
                            left: Box::new(name("left", 109)),
                            operator: BinaryOperator::Equal,
                            right: Box::new(literal(110)),
                        },
                        span: span(111),
                    }),
                    when_true: Box::new(Expression {
                        kind: ExpressionKind::Try {
                            operand: Box::new(Expression {
                                kind: ExpressionKind::Call {
                                    callee: Box::new(name("Enum<T, U>.Case", 112)),
                                    type_arguments: vec![
                                        type_ref("Call<T>", 113),
                                        type_ref("CallArray<U>[]", 114),
                                    ],
                                    arguments: vec![Expression {
                                        kind: ExpressionKind::ArrayLiteral(vec![
                                            Expression {
                                                kind: ExpressionKind::NewArray {
                                                    element_type: type_ref("Element<T>", 115),
                                                    length: Box::new(literal(116)),
                                                },
                                                span: span(117),
                                            },
                                            name("arrayValue", 118),
                                        ]),
                                        span: span(119),
                                    }],
                                },
                                span: span(120),
                            }),
                        },
                        span: span(121),
                    }),
                    when_false: Box::new(Expression {
                        kind: ExpressionKind::Cast {
                            target: type_ref("Cast<U>", 122),
                            operand: Box::new(Expression {
                                kind: ExpressionKind::IncrementDecrement {
                                    operator: IncrementOperator::Increment,
                                    prefix: false,
                                    operand: Box::new(name("counter", 123)),
                                },
                                span: span(124),
                            }),
                        },
                        span: span(125),
                    }),
                },
                span: span(126),
            }),
        },
        span: span(127),
    }
}

fn nested_block(position: usize) -> Block {
    Block {
        statements: vec![Statement::Expression(name("nested", position))],
        span: span(position + 1),
    }
}

#[allow(clippy::too_many_lines)]
fn fixture() -> Module {
    let body = Block {
        statements: vec![
            Statement::Variable(VariableDeclaration {
                visibility: None,
                kind: VariableKind::Explicit(type_ref("Local<T>", 20)),
                name: "local".to_owned(),
                initializer: Some(expression_tree()),
                span: span(21),
            }),
            Statement::Variable(VariableDeclaration {
                visibility: None,
                kind: VariableKind::Constant(type_ref("Constant<U>", 22)),
                name: "constant".to_owned(),
                initializer: Some(literal(23)),
                span: span(24),
            }),
            Statement::Variable(VariableDeclaration {
                visibility: None,
                kind: VariableKind::Inferred,
                name: "inferred".to_owned(),
                initializer: Some(name("source", 25)),
                span: span(26),
            }),
            Statement::If {
                condition: name("ifCondition", 27),
                then_block: nested_block(28),
                else_block: Some(nested_block(30)),
                span: span(32),
            },
            Statement::While {
                condition: name("whileCondition", 33),
                body: nested_block(34),
                span: span(36),
            },
            Statement::For {
                initializer: Some(Box::new(Statement::Variable(VariableDeclaration {
                    visibility: None,
                    kind: VariableKind::Explicit(type_ref("Iterator<T>", 37)),
                    name: "iterator".to_owned(),
                    initializer: Some(literal(38)),
                    span: span(39),
                }))),
                condition: Some(name("forCondition", 40)),
                update: Some(name("forUpdate", 41)),
                body: nested_block(42),
                span: span(44),
            },
            Statement::Switch {
                value: name("choice", 45),
                cases: vec![SwitchCase {
                    enum_name: Some("Choice<T>".to_owned()),
                    case_name: "Some".to_owned(),
                    bindings: vec!["binding".to_owned()],
                    body: nested_block(46),
                    span: span(48),
                }],
                default: Some(nested_block(49)),
                span: span(51),
            },
            Statement::Break(span(52)),
            Statement::Continue(span(53)),
            Statement::Return {
                value: Some(name("result", 54)),
                span: span(55),
            },
        ],
        span: span(56),
    };
    let function = FunctionDeclaration {
        constructor: false,
        is_static: false,
        is_async: false,
        type_parameters: vec![TypeParameter {
            name: "U".to_owned(),
            span: span(57),
        }],
        visibility: Visibility::Public,
        return_type: type_ref("Return<T, U>[]", 58),
        name: "Transform".to_owned(),
        parameters: vec![Parameter {
            type_ref: type_ref("Parameter<T>", 59),
            name: "input".to_owned(),
            span: span(60),
        }],
        body: Some(body),
        span: span(61),
    };
    let property = Property {
        visibility: Visibility::Public,
        type_ref: type_ref("Property<T>", 62),
        name: "Current".to_owned(),
        getter: Some(Accessor {
            visibility: Visibility::Public,
            explicit_visibility: false,
            body: nested_block(63),
            span: span(65),
        }),
        setter: Some(Accessor {
            visibility: Visibility::Private,
            explicit_visibility: true,
            body: nested_block(66),
            span: span(68),
        }),
        span: span(69),
    };
    let class = TypeDeclaration {
        visibility: Visibility::Public,
        is_static: false,
        name: "Container".to_owned(),
        type_parameters: vec![TypeParameter {
            name: "T".to_owned(),
            span: span(70),
        }],
        interfaces: vec![type_ref("Contract<T[]>", 71)],
        members: vec![
            Member::Field(Field {
                visibility: Visibility::Private,
                type_ref: type_ref("Field<T>[]", 72),
                name: "field".to_owned(),
                initializer: Some(name("fieldInitializer", 73)),
                span: span(74),
            }),
            Member::Method(FunctionDeclaration {
                constructor: true,
                is_static: false,
                is_async: false,
                type_parameters: Vec::new(),
                visibility: Visibility::Public,
                return_type: type_ref("void", 75),
                name: "Container".to_owned(),
                parameters: vec![Parameter {
                    type_ref: type_ref("Constructor<T>", 76),
                    name: "value".to_owned(),
                    span: span(77),
                }],
                body: Some(nested_block(78)),
                span: span(80),
            }),
            Member::Method(function.clone()),
            Member::Property(property),
        ],
        span: span(81),
    };
    Module {
        namespace: Some(NamespaceDeclaration {
            name: "sample".to_owned(),
            span: span(1),
        }),
        usings: vec![UsingDeclaration {
            name: "other".to_owned(),
            span: span(2),
        }],
        items: vec![
            Item::Class(class),
            Item::Struct(TypeDeclaration {
                visibility: Visibility::Internal,
                is_static: false,
                name: "Record".to_owned(),
                type_parameters: Vec::new(),
                interfaces: Vec::new(),
                members: vec![Member::Field(Field {
                    visibility: Visibility::Public,
                    type_ref: type_ref("StructField", 82),
                    name: "value".to_owned(),
                    initializer: None,
                    span: span(83),
                })],
                span: span(84),
            }),
            Item::Interface(TypeDeclaration {
                visibility: Visibility::Public,
                is_static: false,
                name: "Contract".to_owned(),
                type_parameters: Vec::new(),
                interfaces: Vec::new(),
                members: vec![Member::Method(FunctionDeclaration {
                    body: None,
                    ..function.clone()
                })],
                span: span(85),
            }),
            Item::Enum(EnumDeclaration {
                visibility: Visibility::Public,
                name: "Choice".to_owned(),
                type_parameters: vec![TypeParameter {
                    name: "T".to_owned(),
                    span: span(86),
                }],
                cases: vec![EnumCase {
                    name: "Some".to_owned(),
                    fields: vec![Parameter {
                        type_ref: type_ref("EnumPayload<T>", 87),
                        name: "value".to_owned(),
                        span: span(88),
                    }],
                    span: span(89),
                }],
                span: span(90),
            }),
            Item::Function(function),
            Item::Variable(VariableDeclaration {
                visibility: Some(Visibility::Internal),
                kind: VariableKind::Constant(type_ref("Global<T>", 91)),
                name: "global".to_owned(),
                initializer: Some(name("globalInitializer", 92)),
                span: span(93),
            }),
        ],
    }
}

#[derive(Default)]
struct RecordingVisitor {
    items: usize,
    statements: usize,
    expressions: usize,
    type_refs: Vec<String>,
}

impl AstVisitorMut for RecordingVisitor {
    fn visit_item_mut(&mut self, item: &mut Item) {
        self.items += 1;
        walk_item_mut(self, item);
    }

    fn visit_type_declaration_mut(&mut self, declaration: &mut TypeDeclaration) {
        declaration.name.insert_str(0, "decl::");
        walk_type_declaration_mut(self, declaration);
    }

    fn visit_enum_declaration_mut(&mut self, declaration: &mut EnumDeclaration) {
        declaration.name.insert_str(0, "decl::");
        walk_enum_declaration_mut(self, declaration);
    }

    fn visit_function_declaration_mut(&mut self, declaration: &mut FunctionDeclaration) {
        declaration.name.insert_str(0, "decl::");
        walk_function_declaration_mut(self, declaration);
    }

    fn visit_statement_mut(&mut self, statement: &mut Statement) {
        self.statements += 1;
        aster_syntax::visit::walk_statement_mut(self, statement);
    }

    fn visit_switch_case_mut(&mut self, case: &mut SwitchCase) {
        if let Some(owner) = &mut case.enum_name {
            owner.insert_str(0, "ref::");
        }
        walk_switch_case_mut(self, case);
    }

    fn visit_expression_mut(&mut self, expression: &mut Expression) {
        self.expressions += 1;
        match &mut expression.kind {
            ExpressionKind::Name(value) => value.insert_str(0, "ref::"),
            ExpressionKind::StructLiteral { type_name, .. }
            | ExpressionKind::NewObject { type_name, .. } => type_name.insert_str(0, "ref::"),
            _ => {}
        }
        walk_expression_mut(self, expression);
    }

    fn visit_type_ref_mut(&mut self, type_ref: &mut TypeRef) {
        self.type_refs.push(type_ref.name.clone());
        type_ref.name.insert_str(0, "type::");
    }
}

#[test]
fn noop_traversal_preserves_every_ast_field() {
    struct Noop;
    impl AstVisitorMut for Noop {}

    let mut module = fixture();
    let original = module.clone();
    Noop.visit_module_mut(&mut module);
    assert_eq!(module, original);
}

#[test]
fn shared_traversal_reaches_every_current_child_shape() {
    let mut module = fixture();
    let mut visitor = RecordingVisitor::default();
    visitor.visit_module_mut(&mut module);

    assert_eq!(visitor.items, 6);
    assert_eq!(visitor.statements, 37);
    assert_eq!(visitor.expressions, 79);
    visitor.type_refs.sort();
    assert_eq!(
        visitor.type_refs,
        [
            "Call<T>",
            "Call<T>",
            "CallArray<U>[]",
            "CallArray<U>[]",
            "Cast<U>",
            "Cast<U>",
            "Constant<U>",
            "Constant<U>",
            "Constructor<T>",
            "Contract<T[]>",
            "Element<T>",
            "Element<T>",
            "EnumPayload<T>",
            "Field<T>[]",
            "Global<T>",
            "Iterator<T>",
            "Iterator<T>",
            "Local<T>",
            "Local<T>",
            "Parameter<T>",
            "Parameter<T>",
            "Parameter<T>",
            "Property<T>",
            "Return<T, U>[]",
            "Return<T, U>[]",
            "Return<T, U>[]",
            "StructField",
            "void",
        ]
    );

    assert_eq!(module.items[0].span(), span(81));
    assert!(matches!(
        &module.items[0],
        Item::Class(declaration)
            if declaration.name == "decl::Container"
                && declaration.interfaces[0].name == "type::Contract<T[]>"
    ));
    assert!(matches!(
        &module.items[3],
        Item::Enum(declaration) if declaration.name == "decl::Choice"
    ));
    assert!(
        visitor
            .type_refs
            .iter()
            .all(|name| !name.starts_with("type::"))
    );
}
