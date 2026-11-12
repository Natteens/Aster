# 01 — Lexical structure

## Objective

Define how source bytes become identifiers, keywords, literals, punctuation, and
comments before parsing. This proposal extends beyond the bootstrap lexer.

## Proposed syntax

```aster
// A line comment
int playerCount = 10;
float gravity = 9.81;
string greeting = "Hello, Aster";
bool enabled = true;
```

## Proposed rules

- Source files use UTF-8 and the `.aster` extension.
- Spaces, tabs, and line endings separate tokens but are otherwise insignificant.
- Identifiers begin with `_` or a Unicode alphabetic character and continue with `_`
  or Unicode alphanumeric characters.
- Keywords are case-sensitive.
- Accepted reserved words include `namespace`, `using`, `class`, `static`, `struct`, `interface`,
  `enum`, `switch`, `case`, `default`, `public`, `internal`,
  `protected`, `private`, `var`, and `const`, plus the accepted control-flow and type-declaration
  words specified by their chapters (`do` is reserved for `do`-`while`).
- The removed pre-alpha words `module` and `import` remain recognized only so the parser can emit
  focused migration diagnostics; they are not accepted syntax.
- `component`, `system`, `read`, `write`, and `query` are ordinary identifiers. An early ECS
  syntax experiment once reserved some of these words in the lexer; that reservation was
  removed, and no core-language keyword is proposed for them by this lexical chapter. See
  `docs/research/ecs.md`.
- `//` starts a comment that ends at the next line ending. `/*` starts a block comment that ends at
  the first following `*/`; block comments do not nest (as in C#), may span lines, and are
  whitespace. `/*` inside a string or character literal is text. An unterminated block comment is a
  controlled lexer error.
- Implemented numeric literals use decimal digits, optionally followed by the suffixes
  `L`/`l`, `U`/`u`, `UL`/`LU` in either case, `F`/`f`, `D`/`d`, or `M`/`m`. Floating-point
  and decimal literals may contain one decimal point followed by digits.
- Integer literals may also be hexadecimal (`0x`/`0X` followed by `0-9`, `a-f`, `A-F`) or binary
  (`0b`/`0B` followed by `0` or `1`), optionally followed by `U`, `L`, `UL`, or `LU` in either case.
  The `f`, `d`, and `m` suffixes do not apply to them. The prefix is recognized only when the
  literal is exactly `0` followed immediately by the prefix letter. A prefixed literal must have at
  least one digit and its value must fit `ulong`; otherwise the lexer reports a controlled error
  (empty prefix, invalid digit, malformed suffix, or value out of range). The type rules are in
  [types](02-types.md#literals).
- Digit separators follow C#: a `_` may appear between digits of decimal, fractional, exponent, and
  prefixed (`0x`, `0b`) literals, in runs of any length (`1__0` is 10), and directly after the `0x`/`0b`
  prefix (`0x_FF`). A separator may not end the digits, so it cannot precede a decimal point, the
  exponent marker, or a suffix (`1_`, `1_.5`, `1_e3`, `1_f`, `0xFF_u` are errors), and it cannot
  follow a decimal point or the exponent marker/sign (`1._5`, `1e_3`). A leading `_` starts an
  identifier, never a literal. Separators do not change the value.
- An exponent (`e` or `E`, an optional `+` or `-`, and one or more decimal digits) may follow the
  integer or fractional digits of a decimal literal and makes it a real literal: `1e3`, `2.5E-3`,
  `1_0e1_0`. An unsuffixed real literal keeps its existing type (`float`); `f`, `d`, and `m` suffixes
  apply after the exponent, and integer suffixes do not. A missing exponent digit (`1e`, `1e+`) is a
  controlled lexer error.
- Proposed strings use double quotes and proposed character literals use single quotes.
- Punctuation includes `(){}[]`, `,`, `.`, `:`, and `;`; operators are defined with
  expressions rather than silently inferred here.

Octal notation and hexadecimal floating-point literals are not implemented.

## Valid design examples

```aster
int caféCount = 2;
int _temporary = 0;
float ratio = 0.5;
// comment until end of line
/* block comment */
int million = 1_000_000;
float tiny = 2.5E-3;
```

## Invalid design examples

```aster
int 2players = 2;       // identifier cannot begin with a digit
string text = "open;   // unterminated string
int player-count = 1;  // `-` separates tokens; it is not part of an identifier
```

## OPEN QUESTIONS

- **OPEN QUESTION:** Are identifiers normalized to a Unicode normalization form?
- **OPEN QUESTION:** Which escape sequences and raw-string forms exist?
- **OPEN QUESTION:** Are newlines ever syntactically significant?
- **OPEN QUESTION:** Is a byte-order mark accepted at the beginning of a file?
