# Implemented grammar

This document describes syntax implemented by the compiler frontend.
It is intentionally narrower than `docs/specification/`.

Source files are UTF-8. Whitespace and `//` line comments are ignored. Identifiers begin
with `_` or a Unicode alphabetic character and continue with `_` or Unicode alphanumeric
characters. Decimal integer and decimal-point literals, strings, characters, and booleans
are implemented. Strings and characters support `\\n`, `\\r`, `\\t`, `\\\\`, `\\"`, and `\\'`.

```ebnf
compilation-unit    = [ namespace-declaration ] , { using-declaration } ,
                      { declaration } , EOF ;
namespace-declaration = "namespace" , namespace-name , ";" ;
using-declaration   = "using" , namespace-name , ";" ;
namespace-name      = identifier , { "." , identifier } ;
declaration         = [ visibility ] ,
                      ( type-declaration | [ "async" ] , function | namespace-variable ) ;

visibility          = "public" | "internal" | "protected" | "private" ;

type-declaration    = class | static-class | struct | interface | enum ;
class               = "class" , identifier , [ type-parameters ] , [ interface-list ] ,
                      "{" , { class-member } , "}" ;
static-class        = "static" , "class" , identifier ,
                      "{" , { static-class-member } , "}" ;
static-class-member = [ visibility ] , "static" , [ "async" ] ,
                      type , identifier , function-tail ;
interface-list      = ":" , type , { "," , type } ;
struct              = "struct" , identifier , [ type-parameters ] , "{" , { type-member } , "}" ;
interface           = "interface" , identifier , [ type-parameters ] , "{" , { interface-member } , "}" ;
enum                = "enum" , identifier , [ type-parameters ] , "{" ,
                      enum-case , { "," , enum-case } , [ "," ] , "}" ;
enum-case           = identifier , [ parameters ] ;

type-member         = [ visibility ] , [ "static" ] , [ "async" ] , type , identifier ,
                      ( field-tail | property-tail | function-tail ) ;
class-member        = type-member | constructor ;
constructor         = [ visibility ] , identifier , parameters , block ;
interface-member    = [ visibility ] , type , identifier , parameters , ";" ;
field-tail          = [ "=" , expression ] , ";" ;
property-tail       = "{" , accessor , { accessor } , "}" ;
accessor            = [ visibility ] , ( "get" | "set" ) , block ;

function            = type , identifier , [ type-parameters ] , function-tail ;
type-parameters     = "<" , identifier , { "," , identifier } , ">" ;
function-tail       = parameters , block ;
parameters          = "(" , [ parameter , { "," , parameter } ] , ")" ;
parameter           = type , identifier ;
type-arguments      = "<" , type , { "," , type } , ">" ;

namespace-variable  = variable-declaration ;
variable-declaration = explicit-variable | inferred-variable | constant ;
explicit-variable  = type , identifier , [ "=" , expression ] , ";" ;
inferred-variable  = "var" , identifier , [ "=" , expression ] , ";" ;
constant            = "const" , type , identifier , [ "=" , expression ] , ";" ;

block               = "{" , { statement } , "}" ;
statement           = variable-declaration
                    | "return" , [ expression ] , ";"
                    | if-statement
                    | while-statement
                    | for-statement
                    | foreach-statement
                    | switch-statement
                    | "break" , ";"
                    | "continue" , ";"
                    | expression , ";" ;

if-statement        = "if" , "(" , expression , ")" , block ,
                      [ "else" , ( if-statement | block ) ] ;
while-statement     = "while" , "(" , expression , ")" , block ;
for-statement       = "for" , "(" , [ for-initializer ] , ";" ,
                      [ expression ] , ";" , [ expression ] , ")" , block ;
for-initializer     = type , identifier , [ "=" , expression ]
                    | "var" , identifier , [ "=" , expression ]
                    | "const" , type , identifier , [ "=" , expression ]
                    | expression ;
foreach-statement   = "foreach" , "(" , type , identifier , "in" ,
                      expression , ")" , block ;
switch-statement    = "switch" , "(" , expression , ")" , "{" ,
                      { switch-case } , [ default-case ] , "}" ;
switch-case         = "case" , [ identifier , "." ] , identifier ,
                      [ "(" , identifiers , ")" ] , ":" , { statement } ;
default-case        = "default" , ":" , { statement } ;

expression          = assignment ;
assignment          = conditional , [ assignment-op , assignment ] ;
conditional         = switch-expression , [ "?" , expression , ":" , assignment ] ;
switch-expression   = logical-or , [ "switch" , "{" , switch-expression-arms , "}" ] ;
switch-expression-arms = ( switch-expression-case , { "," , switch-expression-case } ,
                           [ "," , switch-expression-default ]
                         | switch-expression-default ) , [ "," ] ;
switch-expression-case = [ identifier , "." ] , identifier , [ "(" , identifiers , ")" ] ,
                         "=>" , assignment ;
switch-expression-default = "default" , "=>" , assignment ;
logical-or          = logical-and , { "||" , logical-and } ;
logical-and         = equality , { "&&" , equality } ;
equality            = comparison , { ( "==" | "!=" ) , comparison } ;
comparison          = additive , { ( "<" | "<=" | ">" | ">=" ) , additive } ;
additive            = multiplicative , { ( "+" | "-" ) , multiplicative } ;
multiplicative      = unary , { ( "*" | "/" | "%" ) , unary } ;
unary               = ( "!" | "-" | "++" | "--" | "await" ) , unary | cast | postfix ;
cast                = "(" , value-type , ")" , unary ;
value-type          = "sbyte" | "byte" | "short" | "ushort" | "int" | "uint"
                    | "long" | "ulong" | "float" | "double" | "decimal" | "char" ;
postfix             = primary , { "." , identifier | "[" , expression , "]"
                                | call-suffix | "++" | "--" | try-propagation } ;
call-suffix         = [ type-arguments ] , arguments ;
try-propagation     = "?" ;  (* Result propagation; see below *)
arguments           = "(" , [ expression , { "," , expression } ] , ")" ;
primary             = literal | enum-value | struct-literal | array-literal | new-array | new-object
                    | "this" | identifier | "(" , expression , ")" ;
struct-literal      = named-type , "{" , field-value , { "," , field-value } , "}" ;
field-value         = identifier , ":" , expression ;
array-literal       = "[" , [ expression , { "," , expression } ] , "]" ;
new-array           = "new" , element-type , "[" , expression , "]" ;
new-object          = "new" , named-type , arguments ;
enum-value          = named-type , "." , identifier , [ arguments ] ;
assignment-op       = "=" | "+=" | "-=" | "*=" | "/=" ;
literal             = integer , [ "L" | "l" | "U" | "u" | unsigned-long-suffix ]
                    | ( integer | float ) , ( "f" | "F" | "d" | "D" )
                    | ( integer | float ) , ( "m" | "M" )
                    | float | string | character | "true" | "false" ;
unsigned-long-suffix = ( "U" | "u" ) , ( "L" | "l" )
                     | ( "L" | "l" ) , ( "U" | "u" ) ;

type                = element-type , [ "[" , "]" ] | "void" ;
element-type        = "bool" | "sbyte" | "byte" | "short" | "ushort"
                    | "int" | "uint" | "long" | "ulong" | "float" | "double"
                    | "decimal" | "char" | "string" | named-type ;
named-type          = identifier , [ type-arguments ] ;

identifiers         = identifier , { "," , identifier } ;
```

The body of a switch arm ends at the next `case`, `default`, or closing brace.
There is no implicit fallthrough between arms.

`namespace` may appear once at the beginning. Usings follow it and must precede ordinary
declarations. The declaration is optional; when present, it must match the namespace inferred
from the file's directory relative to the project root. The removed `module` and `import` words
produce migration diagnostics.

Assignment and the conditional expression `?:` are right-associative. All other implemented
binary operators are left-associative with precedence shown from lowest to highest in the
grammar. The full precedence order, from lowest to highest, is: assignment, conditional `?:`,
`||`, `&&`, equality, comparison, additive, multiplicative, unary (including prefix `++`/`--`),
and postfix (member access, calls, postfix `++`/`--`, and `?`).

A trailing `?` in postfix position is the `Result` propagation operator
(`docs/reference/result-propagation.md`), distinct from the ternary `?:`. It is
recognized only when the following token cannot begin a ternary consequent, so
`expr?` propagates while `condition ? a : b` still parses as a conditional; `?.`
and `??` are not yet operators and are reported as syntax errors.

`&&` and `||` short-circuit: the right operand is evaluated only when the left operand does not
already decide the result. `?:` requires a `bool` condition, evaluates exactly one branch, and
both branches must have a compatible value type. Prefix `++`/`--` produce the updated value;
postfix forms produce the value observed before the update. The operand must be a mutable
numeric variable: constants, literals, and temporary expression results are rejected.

After a syntax error the parser recovers at statement, member, and declaration boundaries
(`;`, `}`, and declaration/statement keywords), so one malformed statement does not report the
rest of the file as invalid.

## Implemented validation

- Namespace-level declarations default to `internal`; class and struct members default to `private`;
  interface members default to public contract visibility.
- Multiple visibility modifiers are rejected. `private` and `protected` namespace-level declarations
  are rejected. Protected class members produce a diagnostic because no extension model exists.
- Basic and user-declared types, duplicate declarations/members/locals, parameters, calls,
  argument count/types, expressions, assignments, returns, and definite reads are checked.
- Named struct literals require every public field exactly once. Nested finite structs,
  field projections, value copies, parameters and internal aggregate returns are executable.
- Homogeneous fixed-length arrays support literals, zeroed `new T[length]`, checked indexing,
  element writes, reference assignment, parameters, internal returns, read-only `Length`, and
  compiler-known `foreach`.
- `List<T>` supports construction, `Length`, `Add`, `Get`, `RemoveAt`, and fail-fast `foreach`.
  `Dictionary<K, V>` supports its official key types, lookup/update/removal, and entry snapshots.
- Classes support one constructor, arena allocation, reference identity, declaration-order field
  initializers, instance and static methods, explicit properties, implicit instance receivers, and
  explicit `this`. Static fields and auto-properties are not accepted.
- Functions and methods may overload parameter signatures. Exact matches precede safe implicit
  conversions; ties are errors, and return types do not distinguish overloads.
- Equality compares scalar and string values, comparable struct fields recursively, array/class
  reference identity, and the underlying object identity of interfaces.
- Immutable strings support `string + string`, `+=`, Unicode-scalar `foreach`, and read-only
  `Length`. Direct string indexing is rejected. Other values are never converted to text implicitly.
- `var` requires and infers from an initializer. `const` requires an initializer and cannot
  be assigned again. Explicit variables are mutable.
- Classes, structs, interfaces, nominal class interface lists, fields, namespace-level functions, methods,
  and interface signatures
  are represented and validated. Interfaces cannot contain instance fields.
- Value enums may carry typed payloads. Enum switch statements and restricted switch expressions
  evaluate their input once, bind payloads in arm-local scopes, and require complete cases or
  `default`. Expression arms use `=>`, are comma-separated, and produce one compatible value type.
- `Log`, `Log.Warning`, and `Log.Error` accept one `string`. Other logging members are rejected.
- `if`, `while`, and C-style `for` conditions must be `bool`. Blocks and loop initializers create
  lexical scopes. `break` and `continue` are accepted only inside loops.
- Async functions, `Task.Run`, `Task<T>.Wait`, `await`, and the official `Parallel` operations are
  validated against restricted worker-transfer boundaries.
- Non-`void` functions must return on every reachable path. Statements after `return`, `break`,
  or `continue` receive a warning for unreachable code; warnings do not invalidate compilation.

## Unsupported language features

There is no class inheritance, interface inheritance, constructor/operator overloads, static
fields, auto-properties, named/default arguments, general pattern matching, switch guards,
exceptions, `goto`,
or engine lifecycle syntax. Application entry selection is a tooling rule rather than grammar:
`aster run [FILE]` resolves one public static parameterless `Main` returning `void` or `int`, while
`--function NAME` remains the explicit development mode.
