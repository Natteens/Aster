//! Shared mutable traversal for the syntax tree.
//!
//! The `walk_*_mut` functions own the mechanical recursion. Visitors override
//! only nodes whose data or traversal context they need to customize.

use crate::{
    Accessor, Block, EnumCase, EnumDeclaration, Expression, ExpressionKind, Field,
    FieldInitializer, FunctionDeclaration, Item, Member, Module, NamespaceDeclaration, Parameter,
    Property, Statement, SwitchCase, TypeDeclaration, TypeParameter, TypeRef, UsingDeclaration,
    VariableDeclaration, VariableKind,
};

/// A mutable syntax-tree visitor with structural defaults.
pub trait AstVisitorMut {
    fn visit_module_mut(&mut self, module: &mut Module) {
        walk_module_mut(self, module);
    }

    fn visit_namespace_declaration_mut(&mut self, _declaration: &mut NamespaceDeclaration) {}

    fn visit_using_declaration_mut(&mut self, _declaration: &mut UsingDeclaration) {}

    fn visit_item_mut(&mut self, item: &mut Item) {
        walk_item_mut(self, item);
    }

    fn visit_type_declaration_mut(&mut self, declaration: &mut TypeDeclaration) {
        walk_type_declaration_mut(self, declaration);
    }

    fn visit_enum_declaration_mut(&mut self, declaration: &mut EnumDeclaration) {
        walk_enum_declaration_mut(self, declaration);
    }

    fn visit_enum_case_mut(&mut self, case: &mut EnumCase) {
        walk_enum_case_mut(self, case);
    }

    fn visit_member_mut(&mut self, member: &mut Member) {
        walk_member_mut(self, member);
    }

    fn visit_property_mut(&mut self, property: &mut Property) {
        walk_property_mut(self, property);
    }

    fn visit_accessor_mut(&mut self, accessor: &mut Accessor) {
        walk_accessor_mut(self, accessor);
    }

    fn visit_field_mut(&mut self, field: &mut Field) {
        walk_field_mut(self, field);
    }

    fn visit_function_declaration_mut(&mut self, declaration: &mut FunctionDeclaration) {
        walk_function_declaration_mut(self, declaration);
    }

    fn visit_type_parameter_mut(&mut self, _parameter: &mut TypeParameter) {}

    fn visit_parameter_mut(&mut self, parameter: &mut Parameter) {
        walk_parameter_mut(self, parameter);
    }

    fn visit_variable_declaration_mut(&mut self, declaration: &mut VariableDeclaration) {
        walk_variable_declaration_mut(self, declaration);
    }

    fn visit_block_mut(&mut self, block: &mut Block) {
        walk_block_mut(self, block);
    }

    fn visit_statement_mut(&mut self, statement: &mut Statement) {
        walk_statement_mut(self, statement);
    }

    fn visit_switch_case_mut(&mut self, case: &mut SwitchCase) {
        walk_switch_case_mut(self, case);
    }

    fn visit_expression_mut(&mut self, expression: &mut Expression) {
        walk_expression_mut(self, expression);
    }

    fn visit_field_initializer_mut(&mut self, initializer: &mut FieldInitializer) {
        walk_field_initializer_mut(self, initializer);
    }

    fn visit_type_ref_mut(&mut self, _type_ref: &mut TypeRef) {}
}

pub fn walk_module_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, module: &mut Module) {
    if let Some(namespace) = &mut module.namespace {
        visitor.visit_namespace_declaration_mut(namespace);
    }
    for using in &mut module.usings {
        visitor.visit_using_declaration_mut(using);
    }
    for item in &mut module.items {
        visitor.visit_item_mut(item);
    }
}

pub fn walk_item_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, item: &mut Item) {
    match item {
        Item::Class(declaration) | Item::Struct(declaration) | Item::Interface(declaration) => {
            visitor.visit_type_declaration_mut(declaration);
        }
        Item::Enum(declaration) => visitor.visit_enum_declaration_mut(declaration),
        Item::Function(declaration) => visitor.visit_function_declaration_mut(declaration),
        Item::Variable(declaration) => visitor.visit_variable_declaration_mut(declaration),
    }
}

pub fn walk_type_declaration_mut<V: AstVisitorMut + ?Sized>(
    visitor: &mut V,
    declaration: &mut TypeDeclaration,
) {
    for parameter in &mut declaration.type_parameters {
        visitor.visit_type_parameter_mut(parameter);
    }
    for interface in &mut declaration.interfaces {
        visitor.visit_type_ref_mut(interface);
    }
    for member in &mut declaration.members {
        visitor.visit_member_mut(member);
    }
}

pub fn walk_enum_declaration_mut<V: AstVisitorMut + ?Sized>(
    visitor: &mut V,
    declaration: &mut EnumDeclaration,
) {
    for parameter in &mut declaration.type_parameters {
        visitor.visit_type_parameter_mut(parameter);
    }
    for case in &mut declaration.cases {
        visitor.visit_enum_case_mut(case);
    }
}

pub fn walk_enum_case_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, case: &mut EnumCase) {
    for field in &mut case.fields {
        visitor.visit_parameter_mut(field);
    }
}

pub fn walk_member_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, member: &mut Member) {
    match member {
        Member::Field(field) => visitor.visit_field_mut(field),
        Member::Method(method) => visitor.visit_function_declaration_mut(method),
        Member::Property(property) => visitor.visit_property_mut(property),
    }
}

pub fn walk_property_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, property: &mut Property) {
    visitor.visit_type_ref_mut(&mut property.type_ref);
    if let Some(getter) = &mut property.getter {
        visitor.visit_accessor_mut(getter);
    }
    if let Some(setter) = &mut property.setter {
        visitor.visit_accessor_mut(setter);
    }
}

pub fn walk_accessor_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, accessor: &mut Accessor) {
    visitor.visit_block_mut(&mut accessor.body);
}

pub fn walk_field_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, field: &mut Field) {
    visitor.visit_type_ref_mut(&mut field.type_ref);
    if let Some(initializer) = &mut field.initializer {
        visitor.visit_expression_mut(initializer);
    }
}

pub fn walk_function_declaration_mut<V: AstVisitorMut + ?Sized>(
    visitor: &mut V,
    declaration: &mut FunctionDeclaration,
) {
    for parameter in &mut declaration.type_parameters {
        visitor.visit_type_parameter_mut(parameter);
    }
    visitor.visit_type_ref_mut(&mut declaration.return_type);
    for parameter in &mut declaration.parameters {
        visitor.visit_parameter_mut(parameter);
    }
    if let Some(body) = &mut declaration.body {
        visitor.visit_block_mut(body);
    }
}

pub fn walk_parameter_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, parameter: &mut Parameter) {
    visitor.visit_type_ref_mut(&mut parameter.type_ref);
}

pub fn walk_variable_declaration_mut<V: AstVisitorMut + ?Sized>(
    visitor: &mut V,
    declaration: &mut VariableDeclaration,
) {
    match &mut declaration.kind {
        VariableKind::Explicit(type_ref) | VariableKind::Constant(type_ref) => {
            visitor.visit_type_ref_mut(type_ref);
        }
        VariableKind::Inferred => {}
    }
    if let Some(initializer) = &mut declaration.initializer {
        visitor.visit_expression_mut(initializer);
    }
}

pub fn walk_block_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, block: &mut Block) {
    for statement in &mut block.statements {
        visitor.visit_statement_mut(statement);
    }
}

pub fn walk_statement_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, statement: &mut Statement) {
    match statement {
        Statement::Variable(declaration) => visitor.visit_variable_declaration_mut(declaration),
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                visitor.visit_expression_mut(value);
            }
        }
        Statement::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            visitor.visit_expression_mut(condition);
            visitor.visit_block_mut(then_block);
            if let Some(block) = else_block {
                visitor.visit_block_mut(block);
            }
        }
        Statement::While {
            condition, body, ..
        } => {
            visitor.visit_expression_mut(condition);
            visitor.visit_block_mut(body);
        }
        Statement::For {
            initializer,
            condition,
            update,
            body,
            ..
        } => {
            if let Some(initializer) = initializer {
                visitor.visit_statement_mut(initializer);
            }
            if let Some(condition) = condition {
                visitor.visit_expression_mut(condition);
            }
            if let Some(update) = update {
                visitor.visit_expression_mut(update);
            }
            visitor.visit_block_mut(body);
        }
        Statement::Switch {
            value,
            cases,
            default,
            ..
        } => {
            visitor.visit_expression_mut(value);
            for case in cases {
                visitor.visit_switch_case_mut(case);
            }
            if let Some(default) = default {
                visitor.visit_block_mut(default);
            }
        }
        Statement::Expression(expression) => visitor.visit_expression_mut(expression),
        Statement::Break(_) | Statement::Continue(_) => {}
    }
}

pub fn walk_switch_case_mut<V: AstVisitorMut + ?Sized>(visitor: &mut V, case: &mut SwitchCase) {
    visitor.visit_block_mut(&mut case.body);
}

#[allow(clippy::too_many_lines)]
pub fn walk_expression_mut<V: AstVisitorMut + ?Sized>(
    visitor: &mut V,
    expression: &mut Expression,
) {
    match &mut expression.kind {
        ExpressionKind::StructLiteral { fields, .. } => {
            for field in fields {
                visitor.visit_field_initializer_mut(field);
            }
        }
        ExpressionKind::ArrayLiteral(expressions) => {
            for expression in expressions {
                visitor.visit_expression_mut(expression);
            }
        }
        ExpressionKind::NewArray {
            element_type,
            length,
        } => {
            visitor.visit_type_ref_mut(element_type);
            visitor.visit_expression_mut(length);
        }
        ExpressionKind::NewObject { arguments, .. } => {
            for argument in arguments {
                visitor.visit_expression_mut(argument);
            }
        }
        ExpressionKind::Index { array, index } => {
            visitor.visit_expression_mut(array);
            visitor.visit_expression_mut(index);
        }
        ExpressionKind::Member { object, .. } => visitor.visit_expression_mut(object),
        ExpressionKind::Call {
            callee,
            type_arguments,
            arguments,
        } => {
            visitor.visit_expression_mut(callee);
            for argument in type_arguments {
                visitor.visit_type_ref_mut(argument);
            }
            for argument in arguments {
                visitor.visit_expression_mut(argument);
            }
        }
        ExpressionKind::Unary { operand, .. }
        | ExpressionKind::IncrementDecrement { operand, .. }
        | ExpressionKind::Try { operand } => visitor.visit_expression_mut(operand),
        ExpressionKind::Conditional {
            condition,
            when_true,
            when_false,
        } => {
            visitor.visit_expression_mut(condition);
            visitor.visit_expression_mut(when_true);
            visitor.visit_expression_mut(when_false);
        }
        ExpressionKind::Cast { target, operand } => {
            visitor.visit_type_ref_mut(target);
            visitor.visit_expression_mut(operand);
        }
        ExpressionKind::Binary { left, right, .. } => {
            visitor.visit_expression_mut(left);
            visitor.visit_expression_mut(right);
        }
        ExpressionKind::Assignment { target, value, .. } => {
            visitor.visit_expression_mut(target);
            visitor.visit_expression_mut(value);
        }
        ExpressionKind::Literal(_) | ExpressionKind::Name(_) | ExpressionKind::This => {}
    }
}

pub fn walk_field_initializer_mut<V: AstVisitorMut + ?Sized>(
    visitor: &mut V,
    initializer: &mut FieldInitializer,
) {
    visitor.visit_expression_mut(&mut initializer.value);
}
