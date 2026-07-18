use aster_syntax::{ExpressionKind, Item, Literal, Statement, VariableKind, lex, parse};

#[test]
fn parser_preserves_new_primitive_type_names_and_literal_suffixes() {
    let source = r"
        public void Values()
        {
            sbyte signed8 = 1;
            byte unsigned8 = 2;
            short signed16 = 3;
            ushort unsigned16 = 4;
            uint upper_u = 10U;
            ulong upper_ul = 11UL;
            ulong lower_lu = 12lu;
            ulong mixed_lu = 13lU;
            float upper_f = 1.5F;
            double upper_d = 2.5D;
            decimal upper_m = 3.5M;
        }
    ";
    let module = parse(lex(source).expect("numeric source should lex"))
        .expect("numeric source should parse");
    let Item::Function(function) = &module.items[0] else {
        panic!("expected a function");
    };
    let statements = &function.body.as_ref().expect("function body").statements;

    let expected_types = [
        "sbyte", "byte", "short", "ushort", "uint", "ulong", "ulong", "ulong", "float", "double",
        "decimal",
    ];
    let actual_types: Vec<_> = statements
        .iter()
        .map(|statement| {
            let Statement::Variable(variable) = statement else {
                panic!("expected a variable declaration");
            };
            let VariableKind::Explicit(type_ref) = &variable.kind else {
                panic!("expected an explicit type");
            };
            type_ref.name.as_str()
        })
        .collect();
    assert_eq!(actual_types, expected_types);

    let literal = |index: usize| {
        let Statement::Variable(variable) = &statements[index] else {
            panic!("expected a variable declaration");
        };
        let ExpressionKind::Literal(literal) = &variable
            .initializer
            .as_ref()
            .expect("expected initializer")
            .kind
        else {
            panic!("expected a literal initializer");
        };
        literal
    };
    assert_eq!(literal(4), &Literal::UInt("10".into()));
    assert_eq!(literal(5), &Literal::ULong("11".into()));
    assert_eq!(literal(6), &Literal::ULong("12".into()));
    assert_eq!(literal(7), &Literal::ULong("13".into()));
    assert_eq!(literal(8), &Literal::Float("1.5".into()));
    assert_eq!(literal(9), &Literal::Double("2.5".into()));
    assert_eq!(literal(10), &Literal::Decimal("3.5".into()));
}
