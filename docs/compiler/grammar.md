# Implemented grammar

This document describes syntax implemented by the compiler frontend.
It is intentionally narrower than `docs/specification/`.

Source files are UTF-8. Whitespace and `//` line comments are ignored. Identifiers begin
with `_` or a Unicode alphabetic character and continue with `_` or Unicode alphanumeric
characters. Decimal, hexadecimal (`0x`), and binary (`0b`) integer literals, decimal-point
literals, strings, characters, and booleans are implemented. Strings and characters support `\\n`, `\\r`, `\\t`, `\\\\`, `\\"`, and `\\'`.

```ebnf
compilation-unit    = [ namespace-declaration ] , { using-declaration } ,
                      { declaration } , EOF ;
namespace-declaration = "namespace" , namespace-name , ";" ;
using-declaration   = "using" , namespace-name , ";" ;
namespace-name      = identifier , { "." , identifier } ;
declaration         = [ visibility ] ,
                      ( type-declaration | foreign-function | [ "async" ] , function
                      | namespace-variable ) ;

visibility          = "public" | "internal" | "protected" | "private" ;

type-declaration    = class | static-class | struct | interface | enum ;
class               = "class" , identifier , [ type-parameters ] , [ interface-list ] ,
                      { where-clause } , "{" , { class-member } , "}" ;
static-class        = "static" , "class" , identifier ,
                      "{" , { static-class-member } , "}" ;
static-class-member = [ visibility ] , "static" , [ "async" ] ,
                      type , identifier , function-tail ;
interface-list      = ":" , type , { "," , type } ;
struct              = "struct" , identifier , [ type-parameters ] , { where-clause } ,
                      "{" , { type-member } , "}" ;
interface           = "interface" , identifier , [ type-parameters ] , { where-clause } ,
                      "{" , { interface-member } , "}" ;
enum                = "enum" , identifier , [ type-parameters ] , { where-clause } , "{" ,
                      enum-case , { "," , enum-case } , [ "," ] , "}" ;
enum-case           = identifier , [ parameters ] ;

type-member         = [ visibility ] , [ "static" ] , [ "async" ] , type , identifier ,
                      ( field-tail | property-tail | function-tail ) ;
class-member        = type-member | virtual-member | constructor ;
(* `virtual`, `override` and `abstract` are contextual modifiers (an identifier of that
   spelling stays usable as a name). A virtual member is a non-static, non-private method or
   property of a class; `abstract` members have no body and only an `abstract class` declares
   them. `sealed` applies to classes only, so `sealed override` is rejected. *)
virtual-member      = [ visibility ] , ( "virtual" | "override" ) , type , identifier ,
                      ( property-tail | function-tail )
                    | [ visibility ] , "abstract" , type , identifier ,
                      ( abstract-property-tail | parameters , ";" ) ;
abstract-property-tail = "{" , ( "get" | "set" ) , ";" , [ ( "get" | "set" ) , ";" ] , "}" ;
constructor         = [ visibility ] , identifier , parameters , [ base-call ] , block ;
base-call           = ":" , "base" , "(" , [ argument , { "," , argument } ] , ")" ;
interface-member    = [ visibility ] , type , identifier , parameters , ";" ;
field-tail          = [ "=" , expression ] , ";" ;
property-tail       = "{" , accessor , { accessor } , "}" ;
accessor            = [ visibility ] , ( "get" | "set" ) , block ;

function            = type , identifier , [ type-parameters ] , function-tail ;
foreign-function    = "unsafe" , "foreign" , type , identifier ,
                      parameters , ";" ;
type-parameters     = "<" , identifier , { "," , identifier } , ">" ;
(* `where` is contextual: it is a clause opener only between a declaration
   header and its body, so it remains usable as an ordinary identifier. The
   identifier must name a type parameter of the same declaration, each
   parameter accepts at most one clause, and every listed type must be a
   non-generic interface. *)
where-clause        = "where" , identifier , ":" , type , { "," , type } ;
function-tail       = parameters , { where-clause } , executable-body ;
executable-body     = block | "=>" , expression , ";" ;
parameters          = "(" , [ parameter , { "," , parameter } ] , ")" ;
parameter           = type , identifier , [ "=" , expression ] ;
type-arguments      = "<" , type , { "," , type } , ">" ;

namespace-variable  = variable-declaration ;
variable-declaration = explicit-variable | inferred-variable | constant ;
explicit-variable  = type , identifier , [ "=" , expression ] , ";" ;
inferred-variable  = "var" , identifier , [ "=" , expression ] , ";" ;
constant            = "const" , type , identifier , [ "=" , expression ] , ";" ;

block               = "{" , { statement } , "}" ;
statement           = variable-declaration
                    | "return" , [ expression ] , ";"
                    | unsafe-block
                    | if-statement
                    | while-statement
                    | for-statement
                    | foreach-statement
                    | switch-statement
                    | "break" , ";"
                    | "continue" , ";"
                    | expression , ";" ;
unsafe-block        = "unsafe" , block ;

if-statement        = "if" , "(" , expression , ")" , block ,
                      [ "else" , ( if-statement | block ) ] ;
while-statement     = "while" , "(" , expression , ")" , block ;
for-statement       = "for" , "(" , [ for-initializer ] , ";" ,
                      [ expression ] , ";" , [ expression ] , ")" , block ;
for-initializer     = type , identifier , [ "=" , expression ]
                    | "var" , identifier , [ "=" , expression ]
                    | "const" , type , identifier , [ "=" , expression ]
                    | expression ;
foreach-statement   = "foreach" , "(" , ( type | "var" ) , identifier , "in" ,
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
logical-and         = bitwise-or , { "&&" , bitwise-or } ;
bitwise-or          = bitwise-xor , { "|" , bitwise-xor } ;
bitwise-xor         = bitwise-and , { "^" , bitwise-and } ;
bitwise-and         = equality , { "&" , equality } ;
equality            = comparison , { ( "==" | "!=" ) , comparison } ;
comparison          = shift , { ( "<" | "<=" | ">" | ">=" ) , shift } ;
shift               = additive , { ( "<<" | ">>" | ">>>" ) , additive } ;
additive            = multiplicative , { ( "+" | "-" ) , multiplicative } ;
multiplicative      = unary , { ( "*" | "/" | "%" ) , unary } ;
unary               = ( "!" | "-" | "~" | "++" | "--" | "await" ) , unary | cast | postfix ;
cast                = "(" , value-type , ")" , unary ;
value-type          = "sbyte" | "byte" | "short" | "ushort" | "int" | "uint"
                    | "long" | "ulong" | "float" | "double" | "decimal" | "char" ;
                      (* "decimal" parses but is rejected by semantic analysis as reserved *)
postfix             = primary , { "." , identifier | "[" , expression , "]"
                                | call-suffix | "++" | "--" | try-propagation } ;
call-suffix         = [ type-arguments ] , arguments ;
try-propagation     = "?" ;  (* Result propagation; see below *)
arguments           = "(" , [ argument , { "," , argument } ] , ")" ;
argument            = [ identifier , ":" ] , expression ;
primary             = literal | enum-value | struct-literal | array-literal | new-array | new-object
                    | "this" | base-access | identifier | "(" , expression , ")" ;
base-access         = "base" , "." , identifier ;  (* only as `base.Method(...)` or `base.Property`;
                                                     a non-virtual use of the base member *)
struct-literal      = named-type , "{" , field-value , { "," , field-value } , "}" ;
field-value         = identifier , ":" , expression ;
array-literal       = "[" , [ expression , { "," , expression } ] , "]" ;
new-array           = "new" , element-type , "[" , expression , "]" ;
new-object          = "new" , [ named-type ] , arguments ;
enum-value          = named-type , "." , identifier , [ arguments ] ;
assignment-op       = "=" | "+=" | "-=" | "*=" | "/=" | "&=" | "|=" | "^="
                    | "<<=" | ">>=" | ">>>=" ;
literal             = integer , [ "L" | "l" | "U" | "u" | unsigned-long-suffix ]
                    | prefixed-integer
                    | ( integer | float ) , ( "f" | "F" | "d" | "D" )
                    | ( integer | float ) , ( "m" | "M" )
                    | float | string | character | "true" | "false" ;
unsigned-long-suffix = ( "U" | "u" ) , ( "L" | "l" )
                     | ( "L" | "l" ) , ( "U" | "u" ) ;
prefixed-integer     = ( "0x" | "0X" ) , hex-digit , { hex-digit } , [ integer-suffix ]
                     | ( "0b" | "0B" ) , bin-digit , { bin-digit } , [ integer-suffix ] ;
integer-suffix       = "L" | "l" | "U" | "u" | unsigned-long-suffix ;
hex-digit            = "0".."9" | "a".."f" | "A".."F" ;
bin-digit            = "0" | "1" ;  (* the value must fit ulong; see the type specification *)

type                = element-type , { "[" , "]" } | "void" ;
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
`||`, `&&`, `|`, `^`, `&`, equality, comparison, shift, additive, multiplicative, unary (including
`~` and prefix `++`/`--`), and postfix (member access, calls, postfix `++`/`--`, and `?`).

The lexer produces `<<`, `<<=`, `&=`, `|=`, and `^=` as single tokens but never merges `>`
characters, because nested generic argument lists close with consecutive `>` tokens. The expression
parser recognizes `>>`, `>>>`, `>>=`, and `>>>=` only in operator position, from `>` tokens (and a
final `>=`) with no whitespace between them. A comparison accepts only a lone `>` or `>=`.

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

The parser applies one machine-independent depth budget of 64 recursive source constructs across
expressions, type references, and nested statement bodies. Source that exceeds the budget is
rejected at the construct that crosses it with a positioned diagnostic. This compiler resource
limit guarantees that a successfully parsed AST is shallow enough for the existing frontend
visitors; it is not a general pattern-matching or expression-shape restriction.

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
  element writes, nested array types, reference assignment, parameters, internal returns,
  read-only `Length`, and compiler-known `foreach` with explicit or inferred element types.
- An exact expected type can contextualize `[]`, nested empty array literals, and `new()` in
  initializers, assignments, returns, conditional/switch arms, and selected call candidates.
- `List<T>` supports construction, `Length`, `Add`, `Get`, `RemoveAt`, checked index syntax, and
  fail-fast `foreach`.
  `Dictionary<K, V>` supports its official key types, lookup/update/removal, and entry snapshots.
- Classes support one constructor, arena allocation, reference identity, declaration-order field
  initializers, instance and static methods, explicit properties, implicit instance receivers, and
  explicit `this`. Static fields and auto-properties are not accepted.
- Executable namespace functions and class, struct, static, and generic methods may use block or
  expression bodies. Functions and methods may overload parameter signatures, accept positional
  arguments followed by named arguments, and declare trailing compile-time defaults. Exact
  matches precede safe implicit conversions; ties are errors, and return types do not distinguish
  overloads.
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

Class inheritance is single and closed, with virtual dispatch (see [classes](../reference/classes.md)); there is no downcast, interface inheritance, constructor/operator overloads, static
fields, auto-properties, general pattern matching, switch guards,
exceptions, `goto`,
or engine lifecycle syntax. Application entry selection is a tooling rule rather than grammar:
`aster run [FILE]` resolves one public static parameterless `Main` returning `void` or `int`, while
`--function NAME` remains the explicit development mode.
