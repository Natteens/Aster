use super::{
    AstVisitorMut, Expression, ExpressionKind, HashMap, SwitchCase, SwitchExpressionCase, TypeName,
    TypeRef, walk_expression_mut, walk_switch_case_mut, walk_switch_expression_case_mut,
};

pub(super) fn substitutions(parameters: &[String], concrete: &[String]) -> HashMap<String, String> {
    parameters
        .iter()
        .cloned()
        .zip(concrete.iter().cloned())
        .collect()
}

pub(super) fn substitute_name(name: &str, substitutions: &HashMap<String, String>) -> String {
    let Some(mut type_name) = TypeName::parse(name) else {
        return name.to_owned();
    };
    substitute_type_name(&mut type_name, substitutions);
    type_name.to_string()
}

fn substitute_type_name(type_name: &mut TypeName, substitutions: &HashMap<String, String>) {
    if type_name.arguments.is_empty()
        && let Some(replacement) = substitutions.get(&type_name.base)
        && let Some(mut replacement) = TypeName::parse(replacement)
    {
        replacement.array_depth += type_name.array_depth;
        *type_name = replacement;
        return;
    }
    for argument in &mut type_name.arguments {
        substitute_type_name(argument, substitutions);
    }
}

pub(super) struct TypeSubstituter<'a> {
    substitutions: &'a HashMap<String, String>,
}

impl<'a> TypeSubstituter<'a> {
    pub(super) fn new(substitutions: &'a HashMap<String, String>) -> Self {
        Self { substitutions }
    }
}

impl AstVisitorMut for TypeSubstituter<'_> {
    fn visit_expression_mut(&mut self, expression: &mut Expression) {
        match &mut expression.kind {
            ExpressionKind::StructLiteral { type_name, .. }
            | ExpressionKind::NewObject {
                type_name: Some(type_name),
                ..
            } => {
                *type_name = substitute_name(type_name, self.substitutions);
            }
            ExpressionKind::Name(name)
                if TypeName::parse(name)
                    .is_some_and(|type_name| !type_name.arguments.is_empty()) =>
            {
                *name = substitute_name(name, self.substitutions);
            }
            _ => {}
        }
        walk_expression_mut(self, expression);
    }

    fn visit_switch_case_mut(&mut self, case: &mut SwitchCase) {
        if let Some(owner) = &mut case.enum_name {
            *owner = substitute_name(owner, self.substitutions);
        }
        walk_switch_case_mut(self, case);
    }

    fn visit_switch_expression_case_mut(&mut self, case: &mut SwitchExpressionCase) {
        if let Some(owner) = &mut case.enum_name {
            *owner = substitute_name(owner, self.substitutions);
        }
        walk_switch_expression_case_mut(self, case);
    }

    fn visit_type_ref_mut(&mut self, type_ref: &mut TypeRef) {
        type_ref.name = substitute_name(&type_ref.name, self.substitutions);
    }
}
