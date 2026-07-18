use super::{Diagnostic, HashMap, Literal, TypeName, VariableDeclaration, VariableKind};

pub(super) fn infer_type(
    pattern: &str,
    actual: &str,
    parameters: &[String],
    inferred: &mut HashMap<String, String>,
    span: aster_diagnostics::Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (Some(pattern), Some(actual)) = (TypeName::parse(pattern), TypeName::parse(actual)) else {
        return;
    };
    infer_type_name(&pattern, &actual, parameters, inferred, span, diagnostics);
}

fn infer_type_name(
    pattern: &TypeName,
    actual: &TypeName,
    parameters: &[String],
    inferred: &mut HashMap<String, String>,
    span: aster_diagnostics::Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if parameters
        .iter()
        .any(|parameter| parameter == &pattern.base)
        && pattern.arguments.is_empty()
        && !pattern.array
    {
        let concrete = actual.to_string();
        if let Some(previous) = inferred.get(&pattern.base) {
            if previous != &concrete {
                diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "conflicting inference for `{}`: `{previous}` and `{concrete}`",
                            pattern.base
                        ),
                        span,
                    )
                    .with_help("use matching argument types or explicit casts"),
                );
            }
        } else {
            inferred.insert(pattern.base.clone(), concrete);
        }
        return;
    }
    if parameters
        .iter()
        .any(|parameter| parameter == &pattern.base)
        && pattern.arguments.is_empty()
        && pattern.array
        && actual.array
    {
        let mut element = actual.clone();
        element.array = false;
        let concrete = element.to_string();
        if let Some(previous) = inferred.get(&pattern.base) {
            if previous != &concrete {
                diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "conflicting inference for `{}`: `{previous}` and `{concrete}`",
                            pattern.base
                        ),
                        span,
                    )
                    .with_help("use matching argument types or explicit casts"),
                );
            }
        } else {
            inferred.insert(pattern.base.clone(), concrete);
        }
        return;
    }
    if pattern.array != actual.array
        || pattern.base != actual.base
        || pattern.arguments.len() != actual.arguments.len()
    {
        return;
    }
    for (pattern, actual) in pattern.arguments.iter().zip(&actual.arguments) {
        infer_type_name(pattern, actual, parameters, inferred, span, diagnostics);
    }
}

pub(super) fn variable_type(variable: &VariableDeclaration) -> Option<String> {
    match &variable.kind {
        VariableKind::Explicit(type_) | VariableKind::Constant(type_) => Some(type_.name.clone()),
        VariableKind::Inferred => None,
    }
}
pub(super) fn literal_type(value: &Literal) -> String {
    match value {
        Literal::Integer(value) => value.parse::<i32>().map_or("long", |_| "int"),
        Literal::Long(_) => "long",
        Literal::UInt(_) => "uint",
        Literal::ULong(_) => "ulong",
        Literal::Float(_) => "float",
        Literal::Double(_) => "double",
        Literal::Decimal(_) => "decimal",
        Literal::String(_) => "string",
        Literal::Character(_) => "char",
        Literal::Boolean(_) => "bool",
    }
    .to_owned()
}
