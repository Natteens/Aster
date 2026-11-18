//! LANG-005: the parser turns an auto-property into a private backing field
//! (declared where the property is, so initializers keep declaration order)
//! plus trivial accessors, and understands the contextual `init` accessor.

use aster_syntax::{Item, Member, MemberDispatch, Visibility, lex, parse};

fn members(source: &str) -> Vec<Member> {
    let tokens = lex(source).expect("lexes");
    let module = parse(tokens).unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    match module.items.into_iter().next() {
        Some(Item::Class(class) | Item::Struct(class)) => class.members,
        other => panic!("expected a type, found {other:?}"),
    }
}

fn parse_errors(source: &str) -> Vec<String> {
    let tokens = lex(source).expect("lexes");
    parse(tokens)
        .expect_err("expected parse errors")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn an_auto_property_declares_a_backing_field_before_the_property() {
    let members = members("class C { int A = 1; public int P { get; set; } = 2; int B = 3; }");
    let names = members
        .iter()
        .map(|member| match member {
            Member::Field(field) => field.name.clone(),
            Member::Property(property) => format!("prop {}", property.name),
            Member::Method(method) => method.name.clone(),
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["A", "<P>k__BackingField", "prop P", "B"]);
    let Member::Field(backing) = &members[1] else {
        unreachable!()
    };
    assert_eq!(backing.visibility, Visibility::Private);
    assert!(backing.initializer.is_some());
    let Member::Property(property) = &members[2] else {
        unreachable!()
    };
    assert!(property.getter.is_some() && property.setter.is_some());
    assert_eq!(property.dispatch, MemberDispatch::None);
}

#[test]
fn init_is_a_setter_flavour_and_accessor_modifiers_are_kept() {
    let members =
        members("class C { public int P { get; init; } public int Q { get; private set; } }");
    let properties = members
        .iter()
        .filter_map(|member| match member {
            Member::Property(property) => Some(property),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(properties[0].setter.as_ref().expect("init").is_init);
    let private = properties[1].setter.as_ref().expect("set");
    assert!(!private.is_init);
    assert_eq!(private.visibility, Visibility::Private);
}

#[test]
fn explicit_properties_have_no_backing_field() {
    let members = members("class C { public int P { get { return 1; } } }");
    assert_eq!(members.len(), 1);
}

#[test]
fn malformed_auto_properties_are_diagnosed() {
    assert!(
        parse_errors("class C { int P { get; set { } } }")
            .iter()
            .any(|message| message.contains("cannot mix accessors"))
    );
    assert!(
        parse_errors("class C { int P { get { return 1; } } = 3; }")
            .iter()
            .any(|message| message.contains("only an auto-property can have an initializer"))
    );
    assert!(
        parse_errors("class C { virtual int P { get; init; } }")
            .iter()
            .any(|message| message.contains("`init` accessors are not supported yet"))
    );
    assert!(
        parse_errors("class C { int P { get; set; init; } }")
            .iter()
            .any(|message| message.contains("only one setter"))
    );
}
