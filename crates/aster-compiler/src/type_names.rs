use std::fmt;

/// Structural compiler representation of an AST type spelling.
///
/// The parser keeps the source-facing spelling compact in `TypeRef`; every
/// compiler pass that needs to inspect generic arguments uses this type rather
/// than splitting strings independently.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TypeName {
    pub base: String,
    pub arguments: Vec<Self>,
    pub array: bool,
}

impl TypeName {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Parser::new(value).parse()
    }

    pub(crate) fn map_names(&mut self, map: &mut impl FnMut(&str) -> String) {
        self.base = map(&self.base);
        for argument in &mut self.arguments {
            argument.map_names(map);
        }
    }
}

impl fmt::Display for TypeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.base)?;
        if !self.arguments.is_empty() {
            formatter.write_str("<")?;
            for (index, argument) in self.arguments.iter().enumerate() {
                if index != 0 {
                    formatter.write_str(",")?;
                }
                write!(formatter, "{argument}")?;
            }
            formatter.write_str(">")?;
        }
        if self.array {
            formatter.write_str("[]")?;
        }
        Ok(())
    }
}

struct Parser<'a> {
    source: &'a str,
    offset: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, offset: 0 }
    }

    fn parse(mut self) -> Option<TypeName> {
        let result = self.type_name()?;
        (self.offset == self.source.len()).then_some(result)
    }

    fn type_name(&mut self) -> Option<TypeName> {
        let start = self.offset;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'.'))
        {
            self.offset += 1;
        }
        if start == self.offset {
            return None;
        }
        let base = self.source[start..self.offset].to_owned();
        let mut arguments = Vec::new();
        if self.take(b'<') {
            loop {
                arguments.push(self.type_name()?);
                if self.take(b',') {
                    continue;
                }
                if !self.take(b'>') {
                    return None;
                }
                break;
            }
        }
        let array = self.take(b'[') && self.take(b']');
        Some(TypeName {
            base,
            arguments,
            array,
        })
    }

    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.offset).copied()
    }

    fn take(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TypeName;

    #[test]
    fn parses_and_renders_nested_generic_types() {
        let value = "app::Box<app::Pair<int,string[]>>[]";
        assert_eq!(TypeName::parse(value).unwrap().to_string(), value);
    }
}
