//! Central registry of stable diagnostic codes (`ARCH-007`).
//!
//! Every diagnostic emitted by ASTER carries a [`DiagnosticCode`] formatted as
//! `A` followed by four ASCII digits, grouped by compiler layer:
//!
//! - `A1xxx`: Syntax (lexer and parser)
//! - `A2xxx`: Semantic (project, manifest, entry selection, generics, type/semantic checks)
//! - `A3xxx`: Lowering (HIR/MIR lowering and backend-neutral MIR validation)
//! - `A4xxx`: Backend (backend validation, code generation, and runtime execution)
//!
//! Diagnostic codes are unique and never reused.

use std::collections::BTreeSet;
use std::fmt;

/// The compiler layer a diagnostic belongs to (`ARCH-007`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DiagnosticLayer {
    /// Lexical and syntactic diagnostics (`A1xxx`).
    Syntax,
    /// Name resolution, manifest/entry selection, generics, and semantic checks (`A2xxx`).
    Semantic,
    /// HIR/MIR lowering and backend-neutral MIR validation (`A3xxx`).
    Lowering,
    /// Backend validation, code generation, and runtime execution (`A4xxx`).
    Backend,
}

impl DiagnosticLayer {
    #[must_use]
    pub const fn prefix_digit(self) -> u8 {
        match self {
            Self::Syntax => b'1',
            Self::Semantic => b'2',
            Self::Lowering => b'3',
            Self::Backend => b'4',
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::Semantic => "semantic",
            Self::Lowering => "lowering",
            Self::Backend => "backend",
        }
    }
}

/// Sealed diagnostic code identifier (`A` + four digits).
///
/// The inner field is private so external crates can only pass constants
/// defined in this registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DiagnosticCode(&'static str);

impl DiagnosticCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// Returns the architectural layer for this diagnostic code.
    ///
    /// # Panics
    ///
    /// Panics if the code prefix does not identify layer `1`–`4` (all
    /// registry-defined constants are validated by [`validate_registry`]).
    #[must_use]
    pub const fn layer(self) -> DiagnosticLayer {
        match self.0.as_bytes()[1] {
            b'1' => DiagnosticLayer::Syntax,
            b'2' => DiagnosticLayer::Semantic,
            b'3' => DiagnosticLayer::Lowering,
            b'4' => DiagnosticLayer::Backend,
            _ => panic!("diagnostic code has an invalid layer prefix"),
        }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl AsRef<str> for DiagnosticCode {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl PartialEq<str> for DiagnosticCode {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for DiagnosticCode {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Metadata entry for a registered diagnostic code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagnosticCodeMeta {
    pub code: DiagnosticCode,
    pub name: &'static str,
    pub layer: DiagnosticLayer,
    pub summary: &'static str,
}

/// Validation failure returned by [`validate_registry`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    DuplicateCode(&'static str),
    InvalidFormat(&'static str),
    LayerMismatch {
        code: &'static str,
        expected: DiagnosticLayer,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateCode(code) => write!(formatter, "duplicate diagnostic code `{code}`"),
            Self::InvalidFormat(code) => write!(
                formatter,
                "invalid diagnostic code format `{code}` (expected `A[1-4][0-9]{{3}}`)"
            ),
            Self::LayerMismatch { code, expected } => write!(
                formatter,
                "diagnostic code `{code}` does not match layer `{}`",
                expected.label()
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Validate that every entry in `entries` conforms to `A[1-4][0-9]{3}`, matches
/// its declared [`DiagnosticLayer`], and has no duplicate codes.
///
/// # Errors
///
/// Returns [`RegistryError`] if any code has an invalid format, mismatches its
/// declared layer, or appears more than once.
pub fn validate_registry(entries: &[DiagnosticCodeMeta]) -> Result<(), RegistryError> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        let code = entry.code.as_str();
        let bytes = code.as_bytes();
        if bytes.len() != 5
            || bytes[0] != b'A'
            || !matches!(bytes[1], b'1'..=b'4')
            || !bytes[2..5].iter().all(u8::is_ascii_digit)
        {
            return Err(RegistryError::InvalidFormat(code));
        }
        if bytes[1] != entry.layer.prefix_digit() {
            return Err(RegistryError::LayerMismatch {
                code,
                expected: entry.layer,
            });
        }
        if !seen.insert(code) {
            return Err(RegistryError::DuplicateCode(code));
        }
    }
    Ok(())
}

macro_rules! define_diagnostic_codes {
    (
        $(
            $(#[$meta:meta])*
            $name:ident => ($code:literal, $layer:ident, $summary:literal)
        ),* $(,)?
    ) => {
        $(
            $(#[$meta])*
            pub const $name: DiagnosticCode = DiagnosticCode($code);
        )*

        /// Complete table of all registered ASTER diagnostic codes (`ARCH-007`).
        pub const REGISTRY: &[DiagnosticCodeMeta] = &[
            $(
                DiagnosticCodeMeta {
                    code: $name,
                    name: stringify!($name),
                    layer: DiagnosticLayer::$layer,
                    summary: $summary,
                },
            )*
        ];
    };
}

define_diagnostic_codes! {
    // -------------------------------------------------------------------------
    // A1xxx — Syntax (Lexer: A1001–A1099, Parser: A1101–A1199)
    // -------------------------------------------------------------------------
    LEX_UNEXPECTED_CHAR => ("A1001", Syntax, "unexpected character"),
    LEX_UNTERMINATED_INTERP_STRING => ("A1002", Syntax, "unterminated interpolated string literal"),
    LEX_UNMATCHED_INTERP_BRACE => ("A1003", Syntax, "unmatched `}` in interpolated string"),
    LEX_UNSUPPORTED_ESCAPE => ("A1004", Syntax, "unsupported escape sequence"),
    LEX_SEPARATOR_AFTER_EXPONENT => ("A1005", Syntax, "digit separator `_` cannot follow exponent marker"),
    LEX_EXPONENT_NO_DIGITS => ("A1006", Syntax, "exponent has no digits"),
    LEX_INT_SUFFIX_ON_FLOAT => ("A1007", Syntax, "integer suffix is invalid on floating-point literal"),
    LEX_TRAILING_UNDERSCORE => ("A1008", Syntax, "digit separator `_` must be followed by a digit"),
    LEX_PREFIX_NO_DIGITS => ("A1009", Syntax, "prefixed integer literal has no digits"),
    LEX_INVALID_INT_SUFFIX => ("A1010", Syntax, "invalid integer literal suffix"),
    LEX_INVALID_BASE_DIGIT => ("A1011", Syntax, "invalid digit in prefixed integer literal"),
    LEX_PREFIX_INT_OVERFLOW => ("A1012", Syntax, "prefixed integer literal exceeds `ulong` range"),
    LEX_CHAR_LITERAL_LENGTH => ("A1013", Syntax, "character literal must contain exactly one character"),
    LEX_UNTERMINATED_STRING => ("A1014", Syntax, "unterminated string literal"),
    LEX_UNTERMINATED_BLOCK_COMMENT => ("A1015", Syntax, "unterminated block comment"),
    LEX_SEPARATOR_AFTER_DECIMAL => ("A1016", Syntax, "digit separator `_` cannot follow decimal point"),
    LEX_UNTERMINATED_CHAR => ("A1017", Syntax, "unterminated character literal"),

    PARSE_NESTING_LIMIT => ("A1101", Syntax, "source nesting exceeds compiler limit"),
    PARSE_LEGACY_MODULE => ("A1102", Syntax, "`module` was replaced by `namespace`"),
    PARSE_LEGACY_IMPORT => ("A1103", Syntax, "`import` was replaced by `using`"),
    PARSE_ASYNC_ONLY_FREE_FN => ("A1104", Syntax, "only free functions can be `async`"),
    PARSE_STATIC_TOP_LEVEL_CLASS_ONLY => ("A1105", Syntax, "`static` at namespace level is valid only on a class"),
    PARSE_FOREIGN_TOP_LEVEL_FN_ONLY => ("A1106", Syntax, "`unsafe foreign` applies only to a top-level function"),
    PARSE_ABSTRACT_SEALED_CLASS_ONLY => ("A1107", Syntax, "`abstract` and `sealed` are valid only on a class"),
    PARSE_ABSTRACT_AND_SEALED => ("A1108", Syntax, "a class cannot be both `abstract` and `sealed`"),
    PARSE_STATIC_CLASS_ABSTRACT_SEALED => ("A1109", Syntax, "a `static` class cannot be `abstract` or `sealed`"),
    PARSE_ONLY_CLASSES_STATIC => ("A1110", Syntax, "only classes can be static"),
    PARSE_ONLY_CLASSES_INTERFACES => ("A1111", Syntax, "only classes can declare implemented interfaces"),
    PARSE_EXPECTED_ENUM_COMMA => ("A1112", Syntax, "expected `,` between enum cases"),
    PARSE_INTERFACE_DISPATCH_MODIFIER => ("A1113", Syntax, "interface members cannot have dispatch modifiers"),
    PARSE_CTOR_STATIC => ("A1114", Syntax, "constructors cannot be static"),
    PARSE_CTOR_ASYNC => ("A1115", Syntax, "constructors cannot be `async`"),
    PARSE_CTOR_DISPATCH => ("A1116", Syntax, "constructors cannot be `virtual`, `override`, or `abstract`"),
    PARSE_ONLY_METHODS_ASYNC => ("A1117", Syntax, "only methods or functions can be `async`"),
    PARSE_STATIC_PROPERTY => ("A1118", Syntax, "static properties are not implemented"),
    PARSE_STATIC_AUTO_PROPERTY => ("A1119", Syntax, "static auto-properties are not supported yet"),
    PARSE_FIELD_DISPATCH => ("A1120", Syntax, "fields cannot be `virtual`, `override`, or `abstract`"),
    PARSE_STATIC_FIELD_UNIMPLEMENTED => ("A1121", Syntax, "static fields are not implemented"),
    PARSE_TEST_FN_SHAPE => ("A1122", Syntax, "`test` applies only to a parameterless void function"),
    PARSE_FOREIGN_GENERIC => ("A1123", Syntax, "foreign functions cannot be generic"),
    PARSE_CTOR_MULTIPLE_INITIALIZERS => ("A1124", Syntax, "constructor can have only one initializer"),
    PARSE_PROPERTY_DUPLICATE_GETTER => ("A1125", Syntax, "a property can declare only one getter"),
    PARSE_PROPERTY_DUPLICATE_SETTER => ("A1126", Syntax, "a property can declare only one setter"),
    PARSE_PROPERTY_EXPECTED_ACCESSOR => ("A1127", Syntax, "expected `get`, `set`, or `init` accessor"),
    PARSE_AUTO_PROPERTY_MIXED_BODIES => ("A1128", Syntax, "an auto-property cannot mix accessors with and without bodies"),
    PARSE_VIRTUAL_INIT_UNSUPPORTED => ("A1129", Syntax, "`init` accessors are not supported on virtual/override/abstract properties"),
    PARSE_PROPERTY_INIT_AUTO_ONLY => ("A1130", Syntax, "only an auto-property can have an initializer"),
    PARSE_DUPLICATE_WHERE_CLAUSE => ("A1131", Syntax, "duplicate `where` clause for type parameter"),
    PARSE_UNKNOWN_WHERE_TYPE_PARAM => ("A1132", Syntax, "unknown type parameter in `where` clause"),
    PARSE_SWITCH_CASE_AFTER_DEFAULT => ("A1133", Syntax, "case after `default` is unreachable"),
    PARSE_SWITCH_DUPLICATE_DEFAULT => ("A1134", Syntax, "switch can declare only one `default` case"),
    PARSE_SWITCH_EXPECTED_CASE => ("A1135", Syntax, "expected `case` or `default` in switch"),
    PARSE_SWITCH_EXPR_DUPLICATE_DEFAULT => ("A1136", Syntax, "switch expression can declare only one `default` arm"),
    PARSE_SWITCH_EXPR_ARM_AFTER_DEFAULT => ("A1137", Syntax, "switch expression arm after `default` is unreachable"),
    PARSE_SWITCH_EXPR_EXPECTED_COMMA => ("A1138", Syntax, "expected `,` between switch expression arms"),
    PARSE_BASE_REQUIRES_MEMBER => ("A1139", Syntax, "`base` must be followed by a member access"),
    PARSE_EXPECTED_EXPRESSION => ("A1140", Syntax, "expected expression"),
    PARSE_EMPTY_INTERP_EXPR => ("A1141", Syntax, "interpolated expression is empty"),
    PARSE_MALFORMED_INTERP_STRING => ("A1142", Syntax, "malformed interpolated string"),
    PARSE_INTERP_FORMAT_UNSUPPORTED => ("A1143", Syntax, "format specifiers are not supported in string interpolation"),
    PARSE_INTERP_ALIGNMENT_UNSUPPORTED => ("A1144", Syntax, "alignment is not supported in string interpolation"),
    PARSE_INTERP_EXPECTED_RBRACE => ("A1145", Syntax, "expected `}` closing interpolated expression"),
    PARSE_INITIALIZER_EXPECTED_ASSIGN => ("A1146", Syntax, "expected `=` in object initializer"),
    PARSE_NESTED_INITIALIZER_UNSUPPORTED => ("A1147", Syntax, "nested object and collection initializers are not supported yet"),
    PARSE_POSITIONAL_AFTER_NAMED_ARG => ("A1148", Syntax, "positional arguments cannot follow named arguments"),
    PARSE_MULTIPLE_VISIBILITY => ("A1149", Syntax, "only one visibility modifier is allowed"),
    PARSE_EXPECTED_TYPE => ("A1150", Syntax, "expected type"),
    PARSE_DUPLICATE_MODIFIER => ("A1151", Syntax, "duplicate modifier"),
    PARSE_SEALED_OVERRIDE => ("A1152", Syntax, "`sealed override` is not supported"),
    PARSE_SEALED_ON_MEMBER => ("A1153", Syntax, "`sealed` applies to classes, not to members"),
    PARSE_CONFLICTING_DISPATCH_MODIFIER => ("A1154", Syntax, "dispatch modifier cannot be combined with another dispatch modifier"),
    PARSE_BASE_RESERVED_IDENT => ("A1155", Syntax, "`base` is a reserved word"),
    PARSE_EXPECTED_IDENTIFIER => ("A1156", Syntax, "expected identifier"),
    PARSE_EXPECTED_TOKEN => ("A1157", Syntax, "expected token"),
    PARSE_MISPLACED_HEADER_DECL => ("A1158", Syntax, "namespace declarations and usings must appear before declarations"),
    PARSE_EXPECTED_DECLARATION => ("A1159", Syntax, "expected a declaration"),

    // -------------------------------------------------------------------------
    // A2xxx — Semantic & Front-End (Project, Manifest, Entry, Generics, Types)
    // -------------------------------------------------------------------------
    COMPILER_WORKER_FAILED => ("A2001", Semantic, "compiler worker thread failed or panicked"),
    PROJECT_ROOT_PATH_INVALID => ("A2002", Semantic, "could not resolve or read project root source path"),
    PROJECT_ROOT_EXTENSION => ("A2003", Semantic, "expected a file with `.aster` extension"),
    PROJECT_ROOT_NO_PARENT => ("A2004", Semantic, "root source file has no parent directory"),
    PROJECT_TEST_DISCOVERY_FAILED => ("A2005", Semantic, "could not discover test sources"),
    PROJECT_USING_OUTSIDE_ROOT => ("A2006", Semantic, "using resolved outside its package root"),
    PROJECT_MISSING_STDLIB_NAMESPACE => ("A2007", Semantic, "official standard library namespace unavailable"),
    PROJECT_LOAD_ROOT_FILE => ("A2008", Semantic, "could not load root source file"),
    PROJECT_LOAD_NAMESPACE_FILE => ("A2009", Semantic, "could not load namespace source file"),
    PROJECT_INVALID_NAMESPACE_PATH => ("A2010", Semantic, "source file is outside package root for namespace inference"),
    PROJECT_NAMESPACE_MISMATCH => ("A2011", Semantic, "declared namespace does not match directory namespace"),
    PROJECT_RESERVED_ASTER_NAMESPACE => ("A2012", Semantic, "project code cannot declare reserved `aster.*` namespace"),
    PROJECT_STDLIB_USING_PROJECT => ("A2013", Semantic, "standard library namespace cannot use project namespace"),
    PROJECT_DUPLICATE_USING => ("A2014", Semantic, "duplicate using directive"),
    PROJECT_UNKNOWN_NAMESPACE => ("A2015", Semantic, "namespace not found in package or dependencies"),
    PROJECT_AMBIGUOUS_NAMESPACE => ("A2016", Semantic, "namespace provided by more than one package"),
    PROJECT_CIRCULAR_USING => ("A2017", Semantic, "circular using dependency between namespaces"),
    PROJECT_DUPLICATE_LINKED_DECL => ("A2018", Semantic, "declaration provided by more than one package"),
    PROJECT_STDLIB_EXPORT_CONFLICT => ("A2019", Semantic, "local declaration conflicts with official standard library export"),
    PROJECT_AMBIGUOUS_IMPORTED_NAME => ("A2020", Semantic, "imported name is ambiguous across multiple namespaces"),
    PROJECT_INACCESSIBLE_SYMBOL => ("A2021", Semantic, "symbol is not accessible across namespace or package boundary"),

    MANIFEST_READ_ERROR => ("A2030", Semantic, "could not read `Aster.toml`"),
    MANIFEST_INVALID_TOML => ("A2031", Semantic, "invalid TOML syntax in `Aster.toml`"),
    MANIFEST_NOT_A_TABLE => ("A2032", Semantic, "`Aster.toml` must contain a TOML table"),
    MANIFEST_REMOVED_SCHEMA => ("A2033", Semantic, "`Aster.toml` no longer uses a `schema` field"),
    MANIFEST_UNKNOWN_TOP_FIELD => ("A2034", Semantic, "unknown top-level field in `Aster.toml`"),
    MANIFEST_MISSING_PACKAGE => ("A2035", Semantic, "`Aster.toml` does not define `[package]` table"),
    MANIFEST_INVALID_PACKAGE => ("A2036", Semantic, "invalid `[package]` table or field in `Aster.toml`"),
    MANIFEST_INVALID_PACKAGE_NAME => ("A2037", Semantic, "invalid package identifier in `Aster.toml`"),
    MANIFEST_INVALID_DEPENDENCIES => ("A2038", Semantic, "invalid `[dependencies]` table in `Aster.toml`"),
    MANIFEST_INVALID_DEPENDENCY_NAME => ("A2039", Semantic, "invalid dependency identifier in `Aster.toml`"),
    MANIFEST_INVALID_DEPENDENCY_ENTRY => ("A2040", Semantic, "invalid dependency entry or unknown field"),
    MANIFEST_DEPENDENCY_SOURCE_CONFLICT => ("A2041", Semantic, "dependency defines conflicting or missing source"),
    MANIFEST_INVALID_DEPENDENCY_PATH => ("A2042", Semantic, "invalid path dependency field"),
    MANIFEST_INVALID_DEPENDENCY_GIT => ("A2043", Semantic, "invalid Git dependency URL or revision"),
    MANIFEST_INVALID_APPLICATION => ("A2044", Semantic, "invalid `[application]` table or field in `Aster.toml`"),
    MANIFEST_INVALID_APPLICATION_ENTRY => ("A2045", Semantic, "invalid `application.entry` format or method name"),

    FETCH_INVALID_MANIFEST_PATH => ("A2046", Semantic, "fetch requires root `Aster.toml`"),
    FETCH_LOCKFILE_WRITE_FAILED => ("A2047", Semantic, "could not write or remove `Aster.lock`"),
    GRAPH_LOCKFILE_READ_FAILED => ("A2048", Semantic, "could not read or parse `Aster.lock`"),
    GRAPH_MISSING_PACKAGE_NAME => ("A2049", Semantic, "manifest with `[dependencies]` must declare `[package] name`"),
    GRAPH_DEPENDENCY_NOT_DIRECTORY => ("A2050", Semantic, "dependency source path is not a directory"),
    GRAPH_DEPENDENCY_MISSING_MANIFEST => ("A2051", Semantic, "dependency directory has no `Aster.toml`"),
    GRAPH_DEPENDENCY_CYCLE => ("A2052", Semantic, "package dependency cycle"),
    GRAPH_DEPENDENCY_NAME_MISMATCH => ("A2053", Semantic, "dependency resolves to package with different name"),
    GRAPH_DUPLICATE_PACKAGE_IDENTITY => ("A2054", Semantic, "duplicate package identity across dependency roots"),
    GRAPH_PATH_UPDATE_UNSUPPORTED => ("A2055", Semantic, "path dependency cannot be updated via fetch"),
    GRAPH_PATH_MISSING => ("A2056", Semantic, "path dependency directory does not exist"),
    GRAPH_PATH_ESCAPES_GIT_SOURCE => ("A2057", Semantic, "path dependency escapes immutable Git package source"),
    GRAPH_GIT_NOT_LOCKED_OR_STALE => ("A2058", Semantic, "Git dependency missing from or stale in `Aster.lock`"),
    GRAPH_GIT_SOURCE_CONFLICT => ("A2059", Semantic, "Git dependency declared with conflicting sources or fetch failure"),

    APP_NO_APPLICATION_ENTRY => ("A2060", Semantic, "package does not define an `[application]` entry"),
    APP_ENTRY_IN_STDLIB => ("A2061", Semantic, "application entry cannot point into standard library namespace"),
    APP_ENTRY_TYPE_NOT_FOUND => ("A2062", Semantic, "application entry type was not found"),
    APP_ENTRY_CLASS_NOT_PUBLIC => ("A2063", Semantic, "application entry class is not public"),
    APP_MULTIPLE_CONVENTIONAL_ENTRIES => ("A2064", Semantic, "more than one conventional application entry was found"),
    APP_NO_CONVENTIONAL_ENTRY => ("A2065", Semantic, "no application entry was found in root namespace"),
    APP_INVALID_CONVENTIONAL_ENTRY => ("A2066", Semantic, "conventional `Main` cannot be used as application entry"),
    APP_ENTRY_METHOD_NOT_FOUND => ("A2067", Semantic, "configured application entry method was not found"),
    APP_ENTRY_METHOD_AMBIGUOUS => ("A2068", Semantic, "configured application entry resolves to multiple methods"),
    APP_INVALID_MANIFEST_ENTRY => ("A2069", Semantic, "configured manifest application entry method is invalid"),

    INTERFACE_DUPLICATE_BASE => ("A2100", Semantic, "interface listed more than once as base"),
    INTERFACE_UNKNOWN_BASE => ("A2101", Semantic, "unknown base interface"),
    INTERFACE_NON_INTERFACE_BASE => ("A2102", Semantic, "non-interface type cannot be a base interface"),
    INTERFACE_INHERITANCE_CYCLE => ("A2103", Semantic, "interface inheritance cycle"),
    INTERFACE_CONFLICTING_METHOD => ("A2104", Semantic, "interface inherits conflicting method declarations"),
    INTERFACE_CONFLICTING_PROPERTY => ("A2105", Semantic, "interface inherits conflicting property declarations"),

    GENERIC_CONSTRAINT_NOT_SATISFIED => ("A2110", Semantic, "type argument does not satisfy generic constraint"),
    GENERIC_RESERVED_TASK_TEMPLATE => ("A2111", Semantic, "reserved intrinsic `Task` cannot be declared as generic template"),
    GENERIC_RESERVED_LIST_TEMPLATE => ("A2112", Semantic, "reserved built-in `List` cannot be declared as generic template"),
    GENERIC_RESERVED_DICT_TEMPLATE => ("A2113", Semantic, "reserved built-in `Dictionary` cannot be declared as generic template"),
    GENERIC_AMBIGUOUS_METHOD_CALL => ("A2114", Semantic, "call to generic method is ambiguous"),
    GENERIC_OVERLOAD_ARITY_MISMATCH => ("A2115", Semantic, "no generic method overload with given type argument count"),
    GENERIC_AMBIGUOUS_OVERLOAD_METHOD => ("A2116", Semantic, "call to method with generic overloads is ambiguous"),
    GENERIC_NOT_A_GENERIC_FUNCTION => ("A2117", Semantic, "function is not generic"),
    GENERIC_CONFLICTING_INFERENCE => ("A2118", Semantic, "conflicting generic type argument inference"),
    GENERIC_MALFORMED_TYPE => ("A2119", Semantic, "malformed generic type name"),
    GENERIC_LIST_ARITY => ("A2120", Semantic, "`List` expects 1 type argument"),
    GENERIC_LIST_VOID_ELEMENT => ("A2121", Semantic, "`List<void>` is not supported"),
    GENERIC_LIST_DECIMAL_ELEMENT => ("A2122", Semantic, "`List<decimal>` is not supported until `decimal` is executable"),
    GENERIC_DICT_ARITY => ("A2123", Semantic, "`Dictionary` expects 2 type arguments"),
    GENERIC_DICT_INVALID_KEY => ("A2124", Semantic, "unsupported `Dictionary` key type"),
    GENERIC_DICT_VOID_VALUE => ("A2125", Semantic, "`Dictionary<K, void>` is not supported"),
    GENERIC_TYPE_NOT_GENERIC => ("A2126", Semantic, "type arguments applied to non-generic type"),
    GENERIC_TYPE_ARITY_MISMATCH => ("A2127", Semantic, "wrong number of type arguments for generic type"),
    GENERIC_INFINITE_TYPE_EXPANSION => ("A2128", Semantic, "generic type creates an infinitely expanding specialization"),
    GENERIC_CANNOT_INFER_FN_PARAM => ("A2129", Semantic, "cannot infer type parameter for generic function"),
    GENERIC_FN_ARITY_MISMATCH => ("A2130", Semantic, "wrong number of type arguments for generic function"),
    GENERIC_CANNOT_INFER_FN_CONCRETE => ("A2131", Semantic, "cannot infer concrete types for generic function"),
    GENERIC_RECURSIVE_FN_SPECIALIZATION => ("A2132", Semantic, "generic function recursively creates a different specialization"),
    GENERIC_CANNOT_INFER_METHOD_PARAM => ("A2133", Semantic, "cannot infer type parameter for generic method"),
    GENERIC_METHOD_ARITY_MISMATCH => ("A2134", Semantic, "wrong number of type arguments for generic method"),
    GENERIC_CANNOT_INFER_METHOD_CONCRETE => ("A2135", Semantic, "cannot infer concrete types for generic method"),
    GENERIC_RECURSIVE_METHOD_SPECIALIZATION => ("A2136", Semantic, "generic method recursively creates a different specialization"),
    GENERIC_CONSTRAINT_NOT_INTERFACE => ("A2137", Semantic, "generic constraint must be an interface"),
    GENERIC_OPEN_BASE_UNSUPPORTED => ("A2138", Semantic, "generic class deriving from open base is not supported yet"),
    GENERIC_DUPLICATE_FUNCTION => ("A2139", Semantic, "duplicate generic function declaration"),
    GENERIC_DUPLICATE_TYPE => ("A2140", Semantic, "duplicate generic type declaration"),
    GENERIC_DUPLICATE_ENUM => ("A2141", Semantic, "duplicate generic enum declaration"),
    GENERIC_INTERFACE_METHOD_UNIMPLEMENTED => ("A2142", Semantic, "generic interface methods are not implemented"),
    GENERIC_STATIC_CLASS_UNIMPLEMENTED => ("A2143", Semantic, "generic static classes are not implemented"),
    GENERIC_STATIC_METHOD_UNIMPLEMENTED => ("A2144", Semantic, "static methods on generic types are not implemented"),
    GENERIC_DUPLICATE_TYPE_PARAM => ("A2145", Semantic, "duplicate generic type parameter name"),
    GENERIC_DUPLICATE_CONSTRAINT => ("A2146", Semantic, "duplicate constraint in `where` clause"),
    GENERIC_MALFORMED_CONSTRAINT => ("A2147", Semantic, "malformed generic constraint type"),
    GENERIC_UNKNOWN_CONSTRAINT => ("A2148", Semantic, "unknown generic constraint type"),
    GENERIC_CONSTRAINT_ARITY_MISMATCH => ("A2149", Semantic, "wrong number of type arguments in generic interface constraint"),
    GENERIC_DEFAULT_DEPENDENT_PARAM => ("A2150", Semantic, "default for generic-dependent parameter cannot be proven valid"),
    GENERIC_DEFAULT_TYPE_MISMATCH => ("A2151", Semantic, "default parameter value type mismatch"),
    GENERIC_DEFAULT_NON_CONSTANT => ("A2152", Semantic, "parameter default value must be a compile-time constant"),
    GENERIC_DEFAULT_CONST_OVERFLOW => ("A2153", Semantic, "parameter default constant overflows target type"),
    GENERIC_DEFAULT_CONST_DIV_ZERO => ("A2154", Semantic, "parameter default constant divides by zero"),
    GENERIC_REQUIRED_AFTER_OPTIONAL => ("A2155", Semantic, "required parameter cannot follow an optional parameter"),

    SEM_DECIMAL_DEFERRED => ("A2160", Semantic, "`decimal` is reserved and deferred"),
    SEM_RESERVED_CONCURRENCY_TYPE => ("A2161", Semantic, "type name reserved for intrinsic concurrency system"),
    SEM_RESERVED_LIST_TYPE => ("A2162", Semantic, "type name `List` reserved for built-in `List<T>`"),
    SEM_RESERVED_DICTIONARY_TYPE => ("A2163", Semantic, "type name `Dictionary` reserved for built-in `Dictionary<K, V>`"),
    SEM_DUPLICATE_DECLARATION => ("A2164", Semantic, "duplicate top-level declaration"),

    // calls.rs (A2200–A2259)
    SEM_CALL_LIST_CTOR_ARGS => ("A2200", Semantic, "`new List<T>()` takes no arguments"),
    SEM_CALL_DICT_CTOR_ARGS => ("A2201", Semantic, "`new Dictionary<K, V>()` takes no arguments"),
    SEM_CALL_DICT_INVALID_SPEC => ("A2202", Semantic, "invalid concrete `Dictionary<K, V>` specialization"),
    SEM_CALL_UNKNOWN_TYPE => ("A2203", Semantic, "unknown type name"),
    SEM_CALL_NEW_STATIC_CLASS => ("A2204", Semantic, "static class cannot be instantiated with `new`"),
    SEM_CALL_NEW_NON_CLASS => ("A2205", Semantic, "`new` requires a class type"),
    SEM_CALL_NEW_ABSTRACT_CLASS => ("A2206", Semantic, "abstract class cannot be instantiated"),
    SEM_CALL_NO_CONSTRUCTOR => ("A2207", Semantic, "class has no matching constructor"),
    SEM_CALL_PROTECTED_CONSTRUCTOR => ("A2208", Semantic, "constructor is protected"),
    SEM_CALL_PRIVATE_CONSTRUCTOR => ("A2209", Semantic, "constructor is private"),
    SEM_CALL_UNINITIALIZED_VARIABLE => ("A2210", Semantic, "variable used before initialization"),
    SEM_CALL_PROPERTY_UNREADABLE => ("A2211", Semantic, "property cannot be read"),
    SEM_CALL_INSTANCE_IN_STATIC => ("A2212", Semantic, "instance member cannot be used in static context"),
    SEM_CALL_UNKNOWN_NAME => ("A2213", Semantic, "unknown identifier"),
    SEM_CALL_UNKNOWN_ENUM_CASE => ("A2214", Semantic, "enum has no such case"),
    SEM_CALL_ENUM_PAYLOAD_REQUIRED => ("A2215", Semantic, "enum case requires payload arguments"),
    SEM_CALL_NO_MEMBER => ("A2216", Semantic, "type has no such member"),
    SEM_CALL_UNINITIALIZED_FIELD => ("A2217", Semantic, "field used before initialization"),
    SEM_CALL_ABSTRACT_BASE_MEMBER => ("A2218", Semantic, "`base` member is abstract"),
    SEM_CALL_NO_FIELD => ("A2219", Semantic, "type has no such field"),
    SEM_CALL_TASK_PROP_ARGS => ("A2220", Semantic, "`Task` property accepts no arguments"),
    SEM_CALL_WAIT_IN_ASYNC => ("A2221", Semantic, "`Wait()` is not supported inside an `async` function"),
    SEM_CALL_NO_STATIC_METHOD => ("A2222", Semantic, "type has no such static method"),
    SEM_CALL_LIST_METHOD_ARITY => ("A2223", Semantic, "wrong argument count for `List<T>` method"),
    SEM_CALL_LIST_METHOD_ARG_TYPE => ("A2224", Semantic, "invalid argument type for `List<T>` method"),
    SEM_CALL_DICT_METHOD_ARITY => ("A2225", Semantic, "wrong argument count for `Dictionary<K, V>` method"),
    SEM_CALL_DICT_MISSING_STDLIB_TYPE => ("A2226", Semantic, "`Dictionary<K, V>` method requires official `Option`/`Entry` type"),
    SEM_CALL_NO_METHOD => ("A2227", Semantic, "type has no such method"),
    SEM_CALL_INSTANCE_METHOD_NO_RECEIVER => ("A2228", Semantic, "instance method requires an object receiver"),
    SEM_CALL_NOT_CALLABLE => ("A2229", Semantic, "expression is not a known callable"),
    SEM_CALL_FOREIGN_REQUIRES_UNSAFE => ("A2230", Semantic, "foreign call requires an `unsafe` block"),
    SEM_CALL_STRING_METHOD_ARITY => ("A2231", Semantic, "wrong argument count for `string` method"),
    SEM_CALL_ARG_TYPE_MISMATCH => ("A2232", Semantic, "argument type mismatch"),
    SEM_CALL_STRING_TRYPARSE_OPTION => ("A2233", Semantic, "`string.TryParse` requires `aster.core.Option<T>`"),
    SEM_CALL_ARG_COUNT_MISMATCH => ("A2234", Semantic, "wrong number of arguments in call"),
    SEM_CALL_NO_OVERLOAD_MATCH => ("A2235", Semantic, "no overload accepts these argument types"),
    SEM_CALL_AMBIGUOUS_OVERLOAD => ("A2236", Semantic, "call to overloaded callable is ambiguous"),
    SEM_CALL_UNKNOWN_NAMED_ARG => ("A2237", Semantic, "unknown named argument"),
    SEM_CALL_POSITIONAL_AFTER_NAMED => ("A2238", Semantic, "positional argument after named argument"),
    SEM_CALL_TOO_MANY_ARGS => ("A2239", Semantic, "too many arguments for callable"),
    SEM_CALL_DUPLICATE_NAMED_ARG => ("A2240", Semantic, "parameter supplied more than once"),
    SEM_CALL_MISSING_REQUIRED_ARG => ("A2241", Semantic, "missing required argument"),
    SEM_CALL_UNKNOWN_LOG_METHOD => ("A2242", Semantic, "unknown or non-existent `Log` method"),
    SEM_CALL_TASK_RUN_SIGNATURE => ("A2243", Semantic, "`Task.Run` requires a valid static/free function"),
    SEM_CALL_TASK_RESULT_NOT_TRANSFERABLE => ("A2244", Semantic, "`Task<T>` result cannot cross worker boundary"),
    SEM_CALL_TASK_ARG_NOT_TRANSFERABLE => ("A2245", Semantic, "`Task.Run` argument cannot cross worker boundary"),
    SEM_CALL_WAITALL_ARGS => ("A2246", Semantic, "`Task.WaitAll` expects one homogeneous `Task<T>[]` array"),
    SEM_CALL_WAITALL_RESULT_TYPE => ("A2247", Semantic, "`Task.WaitAll` cannot compose `Task<T>` results"),
    SEM_CALL_PARALLEL_FOR_ARITY => ("A2248", Semantic, "`Parallel.For` expects 3 arguments"),
    SEM_CALL_PARALLEL_FOREACH_ARGS => ("A2249", Semantic, "invalid `Parallel.ForEach` arguments"),
    SEM_CALL_PARALLEL_REDUCE_ARGS => ("A2250", Semantic, "invalid `Parallel.Reduce` arguments"),
    SEM_CALL_PARALLEL_REDUCE_OPERATOR => ("A2251", Semantic, "invalid or ambiguous `Parallel.Reduce` operator"),
    SEM_CALL_PARALLEL_BODY => ("A2252", Semantic, "invalid or ambiguous `Parallel` body function"),
    SEM_CALL_ENUM_PAYLOAD_ARITY => ("A2253", Semantic, "wrong payload argument count for enum case"),
    SEM_CALL_ENUM_PAYLOAD_UNKNOWN_NAME => ("A2254", Semantic, "unknown named enum payload argument"),
    SEM_CALL_ENUM_PAYLOAD_DUPLICATE => ("A2255", Semantic, "enum payload argument supplied more than once"),
    SEM_CALL_ENUM_PAYLOAD_MISSING => ("A2256", Semantic, "missing required enum payload argument"),

    // declarations.rs (A2260–A2329)
    SEM_DECL_EMPTY_ENUM => ("A2260", Semantic, "enum must declare at least one case"),
    SEM_DECL_DUPLICATE_ENUM_CASE => ("A2261", Semantic, "duplicate enum case name"),
    SEM_DECL_ENUM_FIELD_DEFAULT => ("A2262", Semantic, "enum payload fields cannot declare default values"),
    SEM_DECL_DUPLICATE_ENUM_PAYLOAD => ("A2263", Semantic, "duplicate enum payload field name"),
    SEM_DECL_VOID_ENUM_PAYLOAD => ("A2264", Semantic, "enum payload cannot have type `void`"),
    SEM_DECL_DUPLICATE_OVERLOAD => ("A2265", Semantic, "duplicate overload with same parameter types"),
    SEM_DECL_STATIC_CLASS_INTERFACE => ("A2266", Semantic, "static class cannot implement instance interfaces"),
    SEM_DECL_INTERFACE_FIELD => ("A2267", Semantic, "interfaces cannot declare instance fields"),
    SEM_DECL_STATIC_CLASS_FIELD => ("A2268", Semantic, "static class cannot declare fields"),
    SEM_DECL_VOID_FIELD => ("A2269", Semantic, "fields cannot have type `void`"),
    SEM_DECL_STATIC_INTERFACE_METHOD => ("A2270", Semantic, "static interface methods are not implemented"),
    SEM_DECL_ASYNC_BODY_REQUIRED => ("A2271", Semantic, "`async` function must have a body"),
    SEM_DECL_INTERFACE_VISIBILITY => ("A2272", Semantic, "required interface members are public by contract"),
    SEM_DECL_CTOR_CLASS_ONLY => ("A2273", Semantic, "constructors are valid only in classes"),
    SEM_DECL_STATIC_CLASS_CTOR => ("A2274", Semantic, "static class cannot declare a constructor"),
    SEM_DECL_STATIC_CLASS_INSTANCE_MEMBER => ("A2275", Semantic, "members of a static class must be static"),
    SEM_DECL_PROPERTY_CLASS_ONLY => ("A2276", Semantic, "properties are currently supported only in classes"),
    SEM_DECL_STRUCT_AUTO_PROP_INIT => ("A2277", Semantic, "struct auto-property initializers are not supported yet"),
    SEM_DECL_STATIC_CLASS_PROPERTY => ("A2278", Semantic, "static-class properties are not implemented"),
    SEM_DECL_VOID_PROPERTY => ("A2279", Semantic, "properties cannot have type `void`"),
    SEM_DECL_EMPTY_PROPERTY => ("A2280", Semantic, "a property requires a getter or setter"),
    SEM_DECL_ACCESSOR_VISIBILITY => ("A2281", Semantic, "accessor cannot be more visible than its property"),
    SEM_DECL_VIRTUAL_ACCESSOR_PRIVATE => ("A2282", Semantic, "accessor of virtual/override/abstract property cannot be private"),
    SEM_DECL_DUPLICATE_INTERFACE => ("A2283", Semantic, "interface implemented more than once"),
    SEM_DECL_UNKNOWN_INTERFACE => ("A2284", Semantic, "unknown interface"),
    SEM_DECL_INTERFACE_MEMBER_MISSING => ("A2285", Semantic, "class does not implement required interface member"),
    SEM_DECL_INTERFACE_MEMBER_MISMATCH => ("A2286", Semantic, "class member does not match interface contract"),
    SEM_DECL_INTERFACE_MEMBER_NOT_PUBLIC => ("A2287", Semantic, "method must be public to implement interface"),
    SEM_DECL_VALUE_TYPE_CYCLE => ("A2288", Semantic, "cyclic value-type containment"),
    SEM_DECL_DUPLICATE_PARAM => ("A2289", Semantic, "duplicate parameter name"),
    SEM_DECL_VOID_PARAM => ("A2290", Semantic, "parameters cannot have type `void`"),
    SEM_DECL_REQUIRED_AFTER_OPTIONAL => ("A2291", Semantic, "required parameter cannot follow an optional parameter"),
    SEM_DECL_FOREIGN_ASYNC => ("A2292", Semantic, "foreign functions cannot be `async`"),
    SEM_DECL_FOREIGN_UNSUPPORTED_RESULT => ("A2293", Semantic, "foreign function result type is not supported"),
    SEM_DECL_FOREIGN_UNSUPPORTED_PARAM => ("A2294", Semantic, "foreign function parameter type is not supported"),
    SEM_DECL_CTOR_UNINITIALIZED_FIELD => ("A2295", Semantic, "constructor does not initialize field"),
    SEM_DECL_MISSING_RETURN => ("A2296", Semantic, "non-void function must return a value on all paths"),
    SEM_DECL_DEFAULT_GENERIC_DEPENDENT => ("A2297", Semantic, "default for generic-dependent parameter cannot be proven valid"),
    SEM_DECL_DEFAULT_NON_CONSTANT => ("A2298", Semantic, "parameter default must be a compile-time constant"),
    SEM_DECL_DEFAULT_CONST_OVERFLOW => ("A2299", Semantic, "parameter default overflows target type"),
    SEM_DECL_DEFAULT_CONST_DIV_ZERO => ("A2300", Semantic, "parameter default divides by zero"),
    SEM_DECL_TEST_TOP_LEVEL_ONLY => ("A2301", Semantic, "test functions must be declared at namespace level"),
    SEM_DECL_TEST_PUBLIC => ("A2302", Semantic, "test functions cannot be public"),
    SEM_DECL_TEST_INVALID_MODIFIER => ("A2303", Semantic, "test function must be synchronous and ordinary"),
    SEM_DECL_TEST_GENERIC => ("A2304", Semantic, "test functions cannot be generic"),
    SEM_DECL_TEST_PARAMS => ("A2305", Semantic, "test functions cannot declare parameters"),
    SEM_DECL_TEST_RETURN => ("A2306", Semantic, "test functions must return `void`"),
    SEM_DECL_TEST_BODY => ("A2307", Semantic, "test functions must have a body"),
    SEM_DECL_ASYNC_PARAMS => ("A2308", Semantic, "`async` function cannot declare parameters in this version"),
    SEM_DECL_ASYNC_INSTANCE_METHOD => ("A2309", Semantic, "`async` instance methods are not supported in this version"),
    SEM_DECL_ASYNC_VOID_TASK => ("A2310", Semantic, "`async Task<void>` is not supported in this version"),
    SEM_DECL_ASYNC_NON_SCALAR_TASK => ("A2311", Semantic, "`async Task<T>` requires a scalar result in this version"),
    SEM_DECL_ASYNC_RETURN_TYPE => ("A2312", Semantic, "`async` function must return `Task<T>`"),
    SEM_DECL_ASYNC_AWAIT_SHAPE => ("A2313", Semantic, "`await` must directly await `Task.Run(...)` in this version"),
    SEM_DECL_ASYNC_AWAIT_COUNT => ("A2314", Semantic, "`async` function must contain exactly one `await` in this version"),
    SEM_DECL_ASYNC_PARALLEL => ("A2315", Semantic, "`Parallel` is not supported inside an `async` function"),
    SEM_DECL_ASYNC_NON_LINEAR_FLOW => ("A2316", Semantic, "`async` function must have linear control flow in this version"),
    SEM_DECL_TOP_LEVEL_VISIBILITY => ("A2317", Semantic, "`private`/`protected` not valid on namespace-level declaration"),
    SEM_DECL_PROTECTED_CLASS_ONLY => ("A2318", Semantic, "`protected` is only valid for class members"),
    SEM_DECL_DUPLICATE_MEMBER => ("A2319", Semantic, "duplicate member declaration"),
    SEM_DECL_MODIFIER_CLASS_ONLY => ("A2320", Semantic, "dispatch modifier is only valid on class members"),
    SEM_DECL_STATIC_DISPATCH => ("A2321", Semantic, "static member cannot have virtual/override/abstract modifier"),
    SEM_DECL_PRIVATE_DISPATCH => ("A2322", Semantic, "private member cannot have virtual/override/abstract modifier"),
    SEM_DECL_ABSTRACT_IN_CONCRETE_CLASS => ("A2323", Semantic, "abstract member declared in non-abstract class"),

    // expressions.rs (A2330–A2399)
    SEM_EXPR_AWAIT_OUTSIDE_ASYNC => ("A2330", Semantic, "`await` is only valid inside an `async` function"),
    SEM_EXPR_AWAIT_COPY_UNSUPPORTED => ("A2331", Semantic, "`await` of `Task.Run` with copied args/result is unsupported"),
    SEM_EXPR_AWAIT_NON_TASK => ("A2332", Semantic, "`await` requires a `Task<T>` operand"),
    SEM_EXPR_TRY_INVALID_OPERAND => ("A2333", Semantic, "`?` requires official `Result<T, E>` or `Option<T>`"),
    SEM_EXPR_TRY_OUTSIDE_FN => ("A2334", Semantic, "`?` cannot be used outside a function"),
    SEM_EXPR_TRY_RETURN_MISMATCH => ("A2335", Semantic, "`?` error/option type incompatible with enclosing function return type"),
    SEM_EXPR_TRY_MALFORMED_STDLIB_TYPE => ("A2336", Semantic, "malformed `aster.core.Result` or `aster.core.Option` layout"),
    SEM_EXPR_INITIALIZER_REQUIRES_CLASS => ("A2337", Semantic, "object initializers require a class"),
    SEM_EXPR_INITIALIZER_DUPLICATE_MEMBER => ("A2338", Semantic, "member initialized more than once in object initializer"),
    SEM_EXPR_TARGET_NEW_NO_EXPECTED => ("A2339", Semantic, "target-typed `new()` requires an expected constructible type"),
    SEM_EXPR_BARE_BASE => ("A2340", Semantic, "`base` can only be used as `base.Member`"),
    SEM_EXPR_NEGATE_UNSIGNED => ("A2341", Semantic, "cannot negate unsigned value"),
    SEM_EXPR_INVALID_UNARY_OP => ("A2342", Semantic, "unary operator is not valid for operand type"),
    SEM_EXPR_INTERP_VOID => ("A2343", Semantic, "`void` expression cannot be used in string interpolation"),
    SEM_EXPR_INTERP_UNSUPPORTED_TYPE => ("A2344", Semantic, "type cannot be used in string interpolation"),
    SEM_EXPR_STRUCT_UNKNOWN => ("A2345", Semantic, "unknown struct type in literal"),
    SEM_EXPR_NOT_A_STRUCT => ("A2346", Semantic, "named field literal requires a struct type"),
    SEM_EXPR_STRUCT_DUPLICATE_FIELD => ("A2347", Semantic, "struct field initialized more than once"),
    SEM_EXPR_STRUCT_PRIVATE_MEMBER => ("A2348", Semantic, "struct field or property is not public"),
    SEM_EXPR_STRUCT_UNKNOWN_FIELD => ("A2349", Semantic, "struct has no such field"),
    SEM_EXPR_STRUCT_MISSING_FIELD => ("A2350", Semantic, "missing field in struct literal"),
    SEM_EXPR_EMPTY_ARRAY_UNINFERRED => ("A2351", Semantic, "cannot infer element type of empty array literal"),
    SEM_EXPR_ARRAY_LENGTH_NOT_INT => ("A2352", Semantic, "array length must have type `int`"),
    SEM_EXPR_ARRAY_NEGATIVE_LENGTH => ("A2353", Semantic, "array length cannot be negative"),
    SEM_EXPR_ARRAY_NO_DEFAULT => ("A2354", Semantic, "array element type has no non-null default value"),
    SEM_EXPR_INDEX_NOT_INT => ("A2355", Semantic, "index must have type `int`"),
    SEM_EXPR_INDEX_NEGATIVE => ("A2356", Semantic, "index cannot be negative"),
    SEM_EXPR_INDEX_OUT_OF_BOUNDS => ("A2357", Semantic, "constant array index out of bounds"),
    SEM_EXPR_NOT_INDEXABLE => ("A2358", Semantic, "type cannot be indexed"),
    SEM_EXPR_INT_LITERAL_RANGE => ("A2359", Semantic, "integer literal out of range for target type"),
    SEM_EXPR_REAL_LITERAL_RANGE => ("A2360", Semantic, "floating-point literal outside finite range"),
    SEM_EXPR_INVALID_CAST => ("A2361", Semantic, "invalid cast between types"),
    SEM_EXPR_INCDEC_NON_NUMERIC_LIST => ("A2362", Semantic, "increment/decrement requires a numeric list element"),
    SEM_EXPR_INCDEC_NOT_MUTABLE => ("A2363", Semantic, "increment/decrement operand must be a mutable variable"),
    SEM_EXPR_INCDEC_INVALID_TYPE => ("A2364", Semantic, "increment/decrement operator is not valid for type"),
    SEM_EXPR_CONDITIONAL_NON_BOOL => ("A2365", Semantic, "`?:` condition must be `bool`"),
    SEM_EXPR_CONDITIONAL_VOID => ("A2366", Semantic, "`?:` branches must produce a value"),
    SEM_EXPR_CONDITIONAL_INCOMPATIBLE => ("A2367", Semantic, "`?:` branches have incompatible types"),
    SEM_EXPR_PATTERN_OPTION_UNSUPPORTED => ("A2368", Semantic, "unwrapping `Option<T>` with `is`/`as` is not supported yet"),
    SEM_EXPR_PATTERN_UNREACHABLE_TYPE => ("A2369", Semantic, "pattern type can never match subject type"),
    SEM_EXPR_PATTERN_TARGET_NOT_CLASS => ("A2370", Semantic, "type pattern target must be a class"),
    SEM_EXPR_PATTERN_SUBJECT_NOT_CLASS => ("A2371", Semantic, "type pattern requires a class-typed subject"),
    SEM_EXPR_AS_MISSING_OPTION_TYPE => ("A2372", Semantic, "`as T` requires `aster.core.Option<T>` specialization"),
    SEM_EXPR_SWITCH_INVALID_SUBJECT => ("A2373", Semantic, "`switch` expression requires enum, integer, `char`, or `string`"),
    SEM_EXPR_SWITCH_EMPTY => ("A2374", Semantic, "switch expression must contain at least one result arm"),
    SEM_EXPR_SWITCH_VOID_ARM => ("A2375", Semantic, "switch expression arms must produce a value"),
    SEM_EXPR_SWITCH_INCOMPATIBLE_ARMS => ("A2376", Semantic, "switch expression arms have incompatible types"),
    SEM_EXPR_STRING_CONCAT_TYPE => ("A2377", Semantic, "string concatenation requires `string` operands"),
    SEM_EXPR_SHIFT_COUNT_TYPE => ("A2378", Semantic, "shift count does not convert to `int`"),
    SEM_EXPR_INVALID_BINARY_OP => ("A2379", Semantic, "binary operator is not valid for operand types"),
    SEM_EXPR_NO_COMMON_TYPE => ("A2380", Semantic, "types have no implicit common type"),
    SEM_EXPR_ASSIGN_READONLY_FOREACH => ("A2381", Semantic, "cannot assign to read-only `foreach` variable"),
    SEM_EXPR_ASSIGN_TEMPORARY_FIELD => ("A2382", Semantic, "cannot assign to a field of a temporary value"),
    SEM_EXPR_COMPOUND_NEEDS_GETTER => ("A2383", Semantic, "compound assignment requires property getter"),
    SEM_EXPR_READONLY_BUILTIN_MEMBER => ("A2384", Semantic, "built-in `Length` or `Capacity` property is read-only"),
    SEM_EXPR_ASSIGN_CONSTANT => ("A2385", Semantic, "cannot assign to a constant"),
    SEM_EXPR_ASSIGN_GET_ONLY_PROP => ("A2386", Semantic, "get-only or read-only property cannot be assigned here"),
    SEM_EXPR_ASSIGN_INIT_ONLY_PROP => ("A2387", Semantic, "`init` property can only be assigned in constructor or initializer"),
    SEM_EXPR_COMPOUND_NARROWING => ("A2388", Semantic, "compound assignment would narrow operand type"),
    SEM_EXPR_COMPOUND_INCOMPATIBLE => ("A2389", Semantic, "compound assignment is not valid for operand types"),
    SEM_EXPR_CONSTANT_OUT_OF_RANGE => ("A2390", Semantic, "constant value does not fit target type"),
    SEM_EXPR_TYPE_MISMATCH => ("A2391", Semantic, "expression type does not match expected type"),
    SEM_EXPR_NULL_REQUIRES_OPTIONAL => ("A2392", Semantic, "`null` requires an optional type"),
    SEM_EXPR_NULL_COALESCE_NON_OPTIONAL => ("A2393", Semantic, "`??` requires an optional left operand"),
    SEM_EXPR_NULL_COALESCE_INCOMPATIBLE => ("A2394", Semantic, "incompatible fallback type for `??`"),
    SEM_EXPR_NULL_CONDITIONAL_NON_OPTIONAL => ("A2395", Semantic, "`?.` requires an optional receiver"),
    SEM_EXPR_NULL_CONDITIONAL_VOID_VALUE => ("A2396", Semantic, "`?.` with a `void` member can only be used as a statement"),
    SEM_EXPR_NULL_CONDITIONAL_NOT_ASSIGNABLE => ("A2397", Semantic, "null-conditional expression cannot be assigned to"),
    SEM_EXPR_NULL_MISSING_CORE_OPTION => ("A2398", Semantic, "optional syntax requires `aster.core.Option<T>`"),

    // inheritance.rs (A2400–A2429)
    SEM_INH_STATIC_BASE_CLASS => ("A2400", Semantic, "static class cannot be a base class"),
    SEM_INH_SEALED_BASE_CLASS => ("A2401", Semantic, "sealed class cannot be derived from"),
    SEM_INH_MULTIPLE_BASE_CLASSES => ("A2402", Semantic, "class cannot have a second base class"),
    SEM_INH_BASE_AFTER_INTERFACE => ("A2403", Semantic, "base class must be listed before interfaces"),
    SEM_INH_NON_CLASS_BASE => ("A2404", Semantic, "base type is not a class"),
    SEM_INH_CYCLE => ("A2405", Semantic, "cyclic class inheritance"),
    SEM_INH_DUPLICATE_BASE_INTERFACE => ("A2406", Semantic, "interface already implemented by base class"),
    SEM_INH_MEMBER_CONFLICT => ("A2407", Semantic, "member conflicts with inherited member"),
    SEM_INH_UNMARKED_OVERRIDE => ("A2408", Semantic, "method hides inherited method with same signature without `override`"),
    SEM_INH_OVERRIDE_TYPE_MISMATCH => ("A2409", Semantic, "override changes inherited property or return type"),
    SEM_INH_OVERRIDE_ACCESSOR_MISMATCH => ("A2410", Semantic, "override adds an accessor not present on base property"),
    SEM_INH_OVERRIDE_NON_VIRTUAL => ("A2411", Semantic, "cannot override non-virtual base member"),
    SEM_INH_OVERRIDE_VISIBILITY => ("A2412", Semantic, "override changes visibility of base member"),
    SEM_INH_MISSING_ABSTRACT_OVERRIDE => ("A2413", Semantic, "concrete class does not override inherited abstract member"),
    SEM_INH_OVERRIDE_NO_TARGET => ("A2414", Semantic, "`override` member has no matching virtual/abstract base member"),
    SEM_INH_BASE_CALL_NO_BASE => ("A2415", Semantic, "`base(...)` requires class to have a base class"),
    SEM_INH_BASE_NO_CTOR => ("A2416", Semantic, "base class has no constructor to call"),
    SEM_INH_BASE_CTOR_PRIVATE => ("A2417", Semantic, "base class constructor is private"),
    SEM_INH_BASE_CTOR_REQUIRED => ("A2418", Semantic, "constructor must call `base(...)` explicitly"),
    SEM_INH_CTOR_DELEGATION_CYCLE => ("A2419", Semantic, "constructors delegate to each other in a cycle through `this(...)`"),
    SEM_INH_BASE_INVALID_CONTEXT => ("A2420", Semantic, "`base` used outside valid instance/base-class context"),
    SEM_INH_PRIVATE_ACCESS => ("A2421", Semantic, "member is private and inaccessible here"),
    SEM_INH_PROTECTED_ACCESS => ("A2422", Semantic, "member is protected and inaccessible here"),

    // nested_concurrency.rs (A2430–A2434)
    SEM_CONC_ASYNC_PARALLEL_CALL => ("A2430", Semantic, "`async` function transitively reaches `Parallel`"),
    SEM_CONC_NESTED_TARGET_DIRECT => ("A2431", Semantic, "concurrency target itself uses nested concurrency"),
    SEM_CONC_NESTED_TARGET_TRANSITIVE => ("A2432", Semantic, "concurrency target transitively reaches nested concurrency"),

    // statements.rs (A2435–A2464)
    SEM_WARN_UNREACHABLE_CODE => ("A2435", Semantic, "unreachable code"),
    SEM_STMT_REF_LOCAL_BEFORE_AWAIT => ("A2436", Semantic, "reference-typed local cannot be declared before `await`"),
    SEM_STMT_NON_SCALAR_BEFORE_AWAIT => ("A2437", Semantic, "non-scalar local cannot be declared before `await`"),
    SEM_STMT_VOID_RETURN_VALUE => ("A2438", Semantic, "`void` function cannot return a value"),
    SEM_STMT_MISSING_RETURN_VALUE => ("A2439", Semantic, "non-void function return requires a value"),
    SEM_STMT_CTOR_RETURN_UNINIT => ("A2440", Semantic, "constructor returns before field is initialized"),
    SEM_STMT_FOREACH_NOT_ITERABLE => ("A2441", Semantic, "type is not iterable in `foreach`"),
    SEM_STMT_FOREACH_ELEMENT_MISMATCH => ("A2442", Semantic, "`foreach` element type mismatch"),
    SEM_STMT_LOOP_CONTROL_OUTSIDE_LOOP => ("A2443", Semantic, "`break` or `continue` outside a loop"),
    SEM_STMT_SWITCH_INVALID_SUBJECT => ("A2444", Semantic, "`switch` requires enum, integer, `char`, or `string`"),
    SEM_STMT_SWITCH_PATTERN_NOT_CONST => ("A2445", Semantic, "switch case on scalar/string must be a constant label"),
    SEM_STMT_SWITCH_NON_CONST_CASE => ("A2446", Semantic, "switch case label must be a constant expression"),
    SEM_STMT_SWITCH_CONST_OVERFLOW => ("A2447", Semantic, "switch case constant overflows type"),
    SEM_STMT_SWITCH_CONST_DIV_ZERO => ("A2448", Semantic, "switch case constant divides by zero"),
    SEM_STMT_SWITCH_CASE_TYPE_MISMATCH => ("A2449", Semantic, "switch case label type mismatch"),
    SEM_STMT_SWITCH_DUPLICATE_CASE => ("A2450", Semantic, "duplicate switch case"),
    SEM_STMT_SWITCH_MISSING_DEFAULT => ("A2451", Semantic, "switch requires a `default` case"),
    SEM_STMT_SWITCH_CASE_WRONG_ENUM => ("A2452", Semantic, "switch case belongs to a different enum"),
    SEM_STMT_SWITCH_UNKNOWN_ENUM_CASE => ("A2453", Semantic, "enum has no such switch case"),
    SEM_STMT_SWITCH_ENUM_BINDING_ARITY => ("A2454", Semantic, "wrong binding count for enum switch case"),
    SEM_WARN_UNREACHABLE_DEFAULT => ("A2455", Semantic, "unreachable `default` case in exhaustive enum switch"),
    SEM_STMT_SWITCH_NON_EXHAUSTIVE => ("A2456", Semantic, "non-exhaustive enum switch"),
    SEM_STMT_CONDITION_NOT_BOOL => ("A2457", Semantic, "control condition must be `bool`"),
    SEM_STMT_VAR_NO_INITIALIZER => ("A2458", Semantic, "`var` requires an initializer"),
    SEM_STMT_CONST_NO_INITIALIZER => ("A2459", Semantic, "constants require an initializer"),
    SEM_STMT_CONST_NON_CONSTANT => ("A2460", Semantic, "constant initializer must be compile-time constant"),
    SEM_STMT_CONST_OVERFLOW => ("A2461", Semantic, "constant expression overflows target type"),
    SEM_STMT_CONST_DIV_ZERO => ("A2462", Semantic, "constant expression divides by zero"),
    SEM_STMT_DUPLICATE_LOCAL => ("A2463", Semantic, "duplicate variable name in scope"),
    SEM_STMT_VOID_VARIABLE => ("A2464", Semantic, "variables cannot have type `void`"),

    // this_confinement.rs (A2465–A2474)
    SEM_THIS_UNINIT_PROPERTY => ("A2465", Semantic, "property used before initialization in constructor"),
    SEM_THIS_ESCAPE_IN_CTOR => ("A2466", Semantic, "`this` cannot escape while object is under construction"),
    SEM_THIS_OUTSIDE_INSTANCE => ("A2467", Semantic, "`this` used outside valid instance context"),
    SEM_THIS_VIRTUAL_CALL_IN_CTOR => ("A2468", Semantic, "constructor calls virtual member on `this`"),
    SEM_THIS_UNCONFINED_CALL_IN_CTOR => ("A2469", Semantic, "constructor calls method that is not this-confined"),
    SEM_THIS_READ_UNASSIGNED_FIELD => ("A2470", Semantic, "helper called by constructor reads unassigned field"),

    // types.rs (A2475–A2479)
    SEM_TYPE_STATIC_CLASS_AS_VALUE => ("A2475", Semantic, "static class cannot be used as a value type"),
    SEM_TYPE_UNKNOWN => ("A2476", Semantic, "unknown type name"),
    SEM_EXPR_FLOAT_LITERAL_SUFFIX => ("A2477", Semantic, "unsuffixed real literal defaults to `double` where `float` is expected"),
    SEM_TYPE_VOID_NULLABLE => ("A2478", Semantic, "`void` cannot be nullable"),

    // -------------------------------------------------------------------------
    // A3xxx — Lowering & MIR Validation
    // -------------------------------------------------------------------------
    MIR_WORKER_FOREIGN_CALL_REJECTED => ("A3001", Lowering, "worker body reaches a foreign call"),
    MIR_WORKER_HOST_IO_REJECTED => ("A3002", Lowering, "worker body reaches a host I/O, clock, or entropy operation"),

    MIR_DUPLICATE_FUNCTION_SYMBOL => ("A3010", Lowering, "duplicate function symbol in MIR"),
    MIR_DUPLICATE_FIELD_SYMBOL => ("A3011", Lowering, "duplicate field symbol in MIR"),
    MIR_DUPLICATE_STRUCT_FIELD_SYMBOL => ("A3012", Lowering, "duplicate struct field symbol in MIR"),
    MIR_DUPLICATE_CLASS_FIELD_SYMBOL => ("A3013", Lowering, "duplicate class field symbol in MIR"),
    MIR_DUPLICATE_ENUM_CASE_SYMBOL => ("A3014", Lowering, "duplicate enum case symbol in MIR"),
    MIR_DUPLICATE_ENUM_FIELD_SYMBOL => ("A3015", Lowering, "duplicate enum field symbol in MIR"),
    MIR_DUPLICATE_NOMINAL_SYMBOL => ("A3016", Lowering, "duplicate nominal type symbol in MIR"),
    MIR_CLASS_ABSTRACT_AND_SEALED => ("A3017", Lowering, "MIR class is both abstract and sealed"),
    MIR_CLASS_INVALID_BASE => ("A3018", Lowering, "MIR class base is not a declared class"),
    MIR_CLASS_SEALED_BASE => ("A3019", Lowering, "MIR class derives from a sealed class"),
    MIR_CLASS_INHERITANCE_CYCLE => ("A3020", Lowering, "MIR class inheritance contains a cycle"),
    MIR_VTABLE_SHORTER_THAN_BASE => ("A3021", Lowering, "virtual table is shorter than base class"),
    MIR_VTABLE_DUPLICATE_METHOD => ("A3022", Lowering, "virtual table lists method twice"),
    MIR_VTABLE_SLOT_MISMATCH => ("A3023", Lowering, "virtual slot does not match inherited slot"),
    MIR_VTABLE_REABSTRACTED_SLOT => ("A3024", Lowering, "virtual slot re-abstracts an implemented slot"),
    MIR_VTABLE_UNKNOWN_IMPL => ("A3025", Lowering, "virtual slot names an unknown implementation"),
    MIR_VTABLE_IMPL_SIGNATURE_MISMATCH => ("A3026", Lowering, "virtual slot implementation does not match slot signature"),
    MIR_CONCRETE_CLASS_ABSTRACT_SLOT => ("A3027", Lowering, "non-abstract class has an abstract virtual slot"),
    MIR_UNKNOWN_NOMINAL_TYPE => ("A3028", Lowering, "function references an unknown or mismatched nominal type"),
    MIR_RETURN_TYPE_MISMATCH => ("A3029", Lowering, "function return value type mismatches declared return type"),
    MIR_MISSING_RETURN_VALUE => ("A3030", Lowering, "non-void function returns no value"),
    MIR_DESTINATION_TYPE_MISMATCH => ("A3031", Lowering, "instruction destination type mismatch"),
    MIR_ASSIGNMENT_TYPE_MISMATCH => ("A3032", Lowering, "assignment value and place type mismatch"),
    MIR_VIRTUAL_CALL_INVALID => ("A3033", Lowering, "virtual call metadata or receiver mismatch"),
    MIR_ALLOCATE_ABSTRACT_CLASS => ("A3034", Lowering, "function allocates an abstract class"),
    MIR_DICTIONARY_ENTRIES_MISMATCH => ("A3035", Lowering, "dictionary entries metadata mismatches receiver"),
    MIR_COUNT_NON_REFERENCE => ("A3036", Lowering, "retain/release applied to non-reference value"),
    MIR_MALFORMED_ENUM_DISCRIMINANT => ("A3037", Lowering, "malformed enum discriminant read"),
    MIR_MALFORMED_ARRAY_LENGTH => ("A3038", Lowering, "malformed array length read"),
    MIR_BITWISE_NOT_UNSUPPORTED_TYPE => ("A3039", Lowering, "bitwise complement on unsupported type"),
    MIR_INVALID_UPCAST => ("A3040", Lowering, "invalid upcast target class"),
    MIR_INVALID_DOWNCAST => ("A3041", Lowering, "invalid downcast between unrelated classes"),
    MIR_MALFORMED_INTERFACE_TEST => ("A3042", Lowering, "malformed interface type test"),
    MIR_MALFORMED_INTERFACE_LOOKUP => ("A3043", Lowering, "malformed interface lookup"),
    MIR_MALFORMED_CLASS_TEST => ("A3044", Lowering, "malformed class type test"),
    MIR_BITWISE_SHIFT_UNSUPPORTED_TYPE => ("A3045", Lowering, "bitwise or shift operation on unsupported type"),
    MIR_UNKNOWN_FUNCTION_VALUE => ("A3046", Lowering, "operand references an unknown function symbol"),
    MIR_UNDECLARED_LOCAL => ("A3047", Lowering, "place references an undeclared local"),
    MIR_NON_STRUCT_FIELD_PLACE => ("A3048", Lowering, "struct field read from non-struct place"),
    MIR_STRUCT_FIELD_OWNER_MISMATCH => ("A3049", Lowering, "struct field does not belong to base"),
    MIR_MALFORMED_ARRAY_INDEX => ("A3050", Lowering, "malformed array index metadata"),
    MIR_NON_CLASS_FIELD_PLACE => ("A3051", Lowering, "object field read from non-class value"),
    MIR_CLASS_FIELD_OWNER_MISMATCH => ("A3052", Lowering, "class field does not belong to object"),
    MIR_NON_ENUM_FIELD_PLACE => ("A3053", Lowering, "enum field read from non-enum place"),
    MIR_ENUM_CASE_OWNER_MISMATCH => ("A3054", Lowering, "enum case does not belong to base"),
    MIR_ENUM_FIELD_CASE_MISMATCH => ("A3055", Lowering, "enum field does not belong to case"),
    MIR_TYPE_MISMATCH => ("A3056", Lowering, "mismatched MIR types"),
    MIR_CONSTANT_TYPE_MISMATCH => ("A3057", Lowering, "constant has incompatible MIR type"),
    MIR_ENUM_CONSTRUCT_SHAPE => ("A3058", Lowering, "malformed EnumConstruct shape"),
    MIR_STRUCT_AGGREGATE_SHAPE => ("A3059", Lowering, "malformed struct Aggregate shape"),
    MIR_UNKNOWN_BASIC_BLOCK => ("A3060", Lowering, "reference to unknown basic block"),
    MIR_DUPLICATE_BASIC_BLOCK => ("A3061", Lowering, "duplicate basic block identifier"),
    MIR_INTERFACE_IMPL_UNKNOWN_CLASS => ("A3062", Lowering, "interface implementation references unknown class"),
    MIR_INTERFACE_IMPL_UNKNOWN_INTERFACE => ("A3063", Lowering, "interface implementation references unknown interface"),
    MIR_INTERFACE_IMPL_DUPLICATE => ("A3064", Lowering, "duplicate interface implementation in MIR"),
    MIR_INTERFACE_IMPL_METHOD_COUNT => ("A3065", Lowering, "interface implementation has wrong method count"),
    MIR_INTERFACE_IMPL_UNKNOWN_METHOD => ("A3066", Lowering, "interface implementation references unknown method"),
    MIR_INTERFACE_IMPL_METHOD_MISMATCH => ("A3067", Lowering, "concrete method does not match interface method"),

    MIR_UNSUPPORTED_GLOBAL_SYMBOL => ("A3070", Lowering, "unsupported module global symbol place"),
    MIR_OWNED_REGION_NESTED_ENTER => ("A3071", Lowering, "nested or duplicate executable owned-region enter"),
    MIR_OWNED_REGION_UNMATCHED_EXIT => ("A3072", Lowering, "owned-region exit without matching enter"),
    MIR_OWNED_REGION_MISMATCHED_EXIT => ("A3073", Lowering, "mismatched executable owned-region exit"),
    MIR_OWNED_REGION_TEMP_RECLAIM => ("A3074", Lowering, "temporary reclaim inside executable owned region"),
    MIR_OWNED_REGION_MISSING_PRODUCER => ("A3075", Lowering, "owned-region checkpoint not followed by producer call"),
    MIR_OWNED_REGION_NON_REF_PRODUCER => ("A3076", Lowering, "owned-region producer has non-reference result"),
    MIR_OWNED_REGION_PERSISTENT_BARRIER => ("A3077", Lowering, "unrelated persistent effect inside executable owned region"),
    MIR_OWNED_REGION_CROSS_BLOCK => ("A3078", Lowering, "executable owned region crosses basic-block boundary"),
    MIR_OWNED_REGION_MALFORMED_CFG => ("A3079", Lowering, "malformed owned-region CFG"),
    MIR_OWNED_REGION_USE_AFTER_RECLAIM => ("A3080", Lowering, "use of reclaimed owned-region local before redefinition"),
    MIR_OWNED_REGION_TERMINATOR_USE => ("A3081", Lowering, "use of reclaimed owned-region local in terminator"),
    MIR_TEMP_SUBREGION_CONCURRENCY => ("A3082", Lowering, "concurrency in executable AARM temporary subregion function"),
    MIR_TEMP_SUBREGION_CFG => ("A3083", Lowering, "malformed or irreducible AARM temporary subregion CFG"),
    MIR_TEMP_SUBREGION_UNREACHABLE_BLOCK => ("A3084", Lowering, "unreachable executable AARM temporary subregion block"),
    MIR_TEMP_SUBREGION_ENTER => ("A3085", Lowering, "nested or duplicate executable AARM temporary subregion enter"),
    MIR_TEMP_SUBREGION_EXIT => ("A3086", Lowering, "unmatched executable AARM temporary subregion exit"),
    MIR_TEMP_SUBREGION_UNSUPPORTED_INST => ("A3087", Lowering, "unsupported instruction or receiver inside AARM temporary subregion"),
    MIR_TEMP_SUBREGION_ACTIVE_EXIT => ("A3088", Lowering, "function exit with active AARM temporary subregion"),
    MIR_TEMP_SUBREGION_JOIN_MISMATCH => ("A3089", Lowering, "inconsistent AARM temporary subregion state at CFG join"),
    MIR_TEMP_SUBREGION_NO_ALLOCATION => ("A3090", Lowering, "executable AARM temporary subregion without Temporary allocation"),

    LOWER_AARM_REQUIRES_UNTRANSFORMED_MIR => ("A3091", Lowering, "AARM subregion pass requires untransformed MIR"),
    LOWER_AARM_UNKNOWN_FUNCTION => ("A3092", Lowering, "validated AARM candidate references unknown function"),
    LOWER_AARM_SNAPSHOT_MISMATCH => ("A3093", Lowering, "validated AARM candidate no longer matches MIR snapshot"),
    LOWER_AARM_RANGE_OVERFLOW => ("A3094", Lowering, "AARM subregion count or cost exceeds addressable range"),
    LOWER_AARM_DUPLICATE_BLOCK => ("A3095", Lowering, "validated AARM subregion no longer has unique basic blocks"),

    // Runtime & execution scheduler (`aster-exec`, A4001–A4099)
    EXEC_TASK_CANCELLED => ("A4001", Backend, "task was cancelled"),
    EXEC_UNKNOWN_SCALAR_TAG => ("A4002", Backend, "unknown scalar kind tag in worker transport"),
    EXEC_INVALID_UNICODE_SCALAR => ("A4003", Backend, "invalid Unicode scalar value in transport"),
    EXEC_PARALLEL_ARRAY_NULL_OR_MISALIGNED => ("A4004", Backend, "`Parallel.ForEach` received a null or misaligned array"),
    EXEC_PARALLEL_ELEMENT_SIZE_MISMATCH => ("A4005", Backend, "`Parallel.ForEach` array element size mismatch"),
    EXEC_PARALLEL_ARRAY_BOUNDS_OR_OVERFLOW => ("A4006", Backend, "parallel array length, index, or memory budget overflow"),
    EXEC_PARALLEL_ARRAY_READ_FAILED => ("A4007", Backend, "`Parallel.ForEach` failed to read array element"),
    EXEC_PARALLEL_RANGE_INVALID => ("A4008", Backend, "`Parallel.For` range end is before start or overflows"),
    EXEC_PARALLEL_WORKER_DISCONNECTED => ("A4009", Backend, "parallel worker disconnected before finishing"),
    EXEC_TASK_ID_OR_TOKEN_EXHAUSTED => ("A4010", Backend, "task handle id or completion token space exhausted"),
    EXEC_TASK_UNKNOWN_HANDLE => ("A4011", Backend, "unknown task handle"),
    EXEC_GOVERNOR_UNAVAILABLE_OR_MISMATCH => ("A4012", Backend, "memory governor unavailable or mismatched"),
    EXEC_ENTITLEMENT_OR_CAPACITY_EXCEEDED => ("A4013", Backend, "memory governor capacity or entitlement exceeded"),
    EXEC_ASYNC_INVALID_STATE => ("A4014", Backend, "invalid async task state or transition"),
    EXEC_ASYNC_FRAME_SLOT_ERROR => ("A4015", Backend, "async frame slot out of range or invalid"),
    EXEC_ASYNC_SPAWN_ERROR => ("A4016", Backend, "invalid async inner task spawn"),
    EXEC_ASYNC_RESULT_ERROR => ("A4017", Backend, "invalid async awaited result or publication"),
    EXEC_ASYNC_PUMP_ERROR => ("A4018", Backend, "async pump re-entry or completion failure"),
    EXEC_ASYNC_NO_STEPS => ("A4019", Backend, "executable program has no async steps"),
    EXEC_ASYNC_TOKEN_MISMATCH => ("A4020", Backend, "async completion token unknown or mismatched"),
    EXEC_POOL_SHUTDOWN => ("A4021", Backend, "execution pool is shut down"),
    EXEC_POOL_INVALID_WORKER_CONFIG => ("A4022", Backend, "invalid execution pool worker configuration or preparation failure"),
    EXEC_TASK_ARG_FRAME_INVALID => ("A4023", Backend, "`Task.Run` argument frame or allocation failed"),
    EXEC_WORKER_DISCONNECTED => ("A4024", Backend, "task worker disconnected before sending result"),
    EXEC_WORKER_EXECUTION_FAILED => ("A4025", Backend, "Aster runtime or test failure"),
    EXEC_PROVIDER_MISSING_FUNCTION => ("A4026", Backend, "requested function or preparation unavailable"),

    // Codegen & JIT backend (`aster-codegen-cranelift`, A4100–A4199)
    BACKEND_ENTRY_NOT_FOUND => ("A4100", Backend, "requested entry or test function symbol not found"),
    BACKEND_ENTRY_NOT_PUBLIC => ("A4101", Backend, "entry function is not public"),
    BACKEND_ENTRY_SIGNATURE_UNSUPPORTED => ("A4102", Backend, "entry or test function has unsupported parameters or return type"),
    BACKEND_DUPLICATE_FUNCTION_SYMBOL => ("A4103", Backend, "duplicate function or foreign symbol in MIR"),
    BACKEND_FOREIGN_ABI_INVALID => ("A4104", Backend, "invalid foreign ABI signature or undeclared foreign binding"),
    BACKEND_INTRINSIC_INVALID_SHAPE => ("A4105", Backend, "malformed runtime or stdlib intrinsic call in MIR"),
    BACKEND_INTRINSIC_MISSING_STDLIB_TYPE => ("A4106", Backend, "intrinsic references missing or malformed stdlib Option/Result/Entry type"),
    BACKEND_UNKNOWN_TYPE_OR_OWNER => ("A4107", Backend, "MIR references unknown struct, class, enum, interface, or owner"),
    BACKEND_MALFORMED_CFG_OR_LOCAL => ("A4108", Backend, "MIR function has malformed CFG, duplicate blocks/locals, or undeclared local"),
    BACKEND_REFCOUNT_INVALID => ("A4109", Backend, "invalid reference-count instruction or unsupported runtime mode"),
    BACKEND_COLLECTION_TYPE_MISMATCH => ("A4110", Backend, "List or Dictionary instruction type/metadata mismatch"),
    BACKEND_UNSUPPORTED_DICTIONARY_KEY => ("A4111", Backend, "unsupported concrete Dictionary key type in backend"),
    BACKEND_STRING_OPERATION_INVALID => ("A4112", Backend, "invalid StringBuilder or string operation in MIR"),
    BACKEND_ARRAY_ALLOCATION_INVALID => ("A4113", Backend, "invalid array allocation or explicit initialization contract"),
    BACKEND_ARRAY_BOUNDS_CONTRACT_INVALID => ("A4114", Backend, "invalid proven array-bounds contract in MIR"),
    BACKEND_ASSIGNMENT_TYPE_MISMATCH => ("A4115", Backend, "assignment or return destination type mismatch in MIR"),
    BACKEND_CALL_SIGNATURE_MISMATCH => ("A4116", Backend, "call, virtual call, or interface call signature mismatch"),
    BACKEND_CALL_RECEIVER_INVALID => ("A4117", Backend, "virtual or interface call has invalid receiver"),
    BACKEND_TYPE_OP_INVALID => ("A4118", Backend, "invalid class/interface cast, test, or lookup in MIR"),
    BACKEND_ARITHMETIC_OR_BRANCH_INVALID => ("A4119", Backend, "invalid division/remainder, branch condition, or integer literal in MIR"),
    BACKEND_TRANSFER_TYPE_INVALID => ("A4120", Backend, "non-transferable type in `Task.Run` or async frame"),
    BACKEND_DECIMAL_UNSUPPORTED => ("A4121", Backend, "`decimal` cannot be represented or executed by the JIT yet"),
    BACKEND_UNSUPPORTED_CONSTRUCT => ("A4122", Backend, "unsupported MIR construct reached Cranelift backend"),
    BACKEND_MISSING_EXECUTION_CONTEXT => ("A4123", Backend, "JIT lowering missing ExecutionContext parameter"),
    BACKEND_MISSING_DESTINATION_OR_RESULT => ("A4124", Backend, "JIT lowering missing required call destination or return value"),
    BACKEND_LAYOUT_OR_OFFSET_OVERFLOW => ("A4125", Backend, "runtime type layout, descriptor, or ABI offset missing or overflows"),
    BACKEND_INHERITANCE_LAYOUT_INVALID => ("A4126", Backend, "class inheritance cycle or invalid descriptor/vtable layout"),
    BACKEND_VALUE_TYPE_LAYOUT_CYCLE => ("A4127", Backend, "recursive struct or enum layout reached the JIT"),
    BACKEND_JIT_INTERNAL_ERROR => ("A4128", Backend, "Cranelift JIT compilation or module emission error"),
    BACKEND_SYMBOL_NOT_PREPARED => ("A4129", Backend, "requested JIT symbol was not prepared"),
    BACKEND_RUNTIME_TRAP_OR_ERROR => ("A4130", Backend, "Aster runtime execution error"),
    BACKEND_GOVERNED_EXECUTION_UNSUPPORTED => ("A4131", Backend, "governed research mode does not support requested execution mix"),
    BACKEND_INVALID_CHAR_RETURN => ("A4132", Backend, "function or worker returned invalid Unicode scalar for `char`"),
    BACKEND_AARM_GOVERNOR_FAILED => ("A4133", Backend, "AARM memory governor discovery or validation failed"),
    BACKEND_SUBREGION_LOWERING_INVALID => ("A4134", Backend, "invalid executable owned-region or temporary-subregion MIR in codegen"),
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticCode, DiagnosticCodeMeta, DiagnosticLayer, LEX_UNEXPECTED_CHAR, REGISTRY,
        RegistryError, validate_registry,
    };

    #[test]
    fn central_registry_is_valid_unique_and_layer_aligned() {
        assert_eq!(validate_registry(REGISTRY), Ok(()));
        assert!(!REGISTRY.is_empty());
        assert_eq!(LEX_UNEXPECTED_CHAR.as_str(), "A1001");
        assert_eq!(LEX_UNEXPECTED_CHAR.layer(), DiagnosticLayer::Syntax);
    }

    #[test]
    fn validate_registry_rejects_duplicate_codes() {
        let entries = [
            DiagnosticCodeMeta {
                code: DiagnosticCode("A1001"),
                name: "FIRST",
                layer: DiagnosticLayer::Syntax,
                summary: "first",
            },
            DiagnosticCodeMeta {
                code: DiagnosticCode("A1001"),
                name: "SECOND",
                layer: DiagnosticLayer::Syntax,
                summary: "duplicate",
            },
        ];
        assert_eq!(
            validate_registry(&entries),
            Err(RegistryError::DuplicateCode("A1001"))
        );
    }

    #[test]
    fn validate_registry_rejects_invalid_formats_and_layer_mismatches() {
        for bad in ["E1001", "A100", "A10001", "A5001", "A0001", "A10x1"] {
            let entries = [DiagnosticCodeMeta {
                code: DiagnosticCode(bad),
                name: "BAD",
                layer: DiagnosticLayer::Syntax,
                summary: "bad format",
            }];
            assert_eq!(
                validate_registry(&entries),
                Err(RegistryError::InvalidFormat(bad)),
                "format `{bad}` should be rejected"
            );
        }

        let mismatched = [DiagnosticCodeMeta {
            code: DiagnosticCode("A2001"),
            name: "MISMATCH",
            layer: DiagnosticLayer::Syntax,
            summary: "wrong layer",
        }];
        assert_eq!(
            validate_registry(&mismatched),
            Err(RegistryError::LayerMismatch {
                code: "A2001",
                expected: DiagnosticLayer::Syntax,
            })
        );
    }
}
