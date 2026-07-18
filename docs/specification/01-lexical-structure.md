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
  words specified by their chapters.
- The removed pre-alpha words `module` and `import` remain recognized only so the parser can emit
  focused migration diagnostics; they are not accepted syntax.
- `component`, `system`, `read`, `write`, and `query` are ordinary identifiers. An early ECS
  syntax experiment once reserved some of these words in the lexer; that reservation was
  removed, and no core-language keyword is proposed for them by this lexical chapter. See
  `docs/future/ecs-package.md`.
- `//` starts a comment that ends at the next line ending.
- Implemented numeric literals use decimal digits, optionally followed by the suffixes
  `L`/`l`, `U`/`u`, `UL`/`LU` in either case, `F`/`f`, `D`/`d`, or `M`/`m`. Floating-point
  and decimal literals may contain one decimal point followed by digits.
- Proposed strings use double quotes and proposed character literals use single quotes.
- Punctuation includes `(){}[]`, `,`, `.`, `:`, and `;`; operators are defined with
  expressions rather than silently inferred here.

Hexadecimal/binary notation, exponents and digit separators are not implemented.

## Valid design examples

```aster
int caféCount = 2;
int _temporary = 0;
float ratio = 0.5;
// comment until end of line
```

## Invalid design examples

```aster
int 2players = 2;       // identifier cannot begin with a digit
string text = "open;   // unterminated string
int player-count = 1;  // `-` separates tokens; it is not part of an identifier
```

## OPEN QUESTIONS

- **OPEN QUESTION:** Are identifiers normalized to a Unicode normalization form?
- **OPEN QUESTION:** Are block comments supported, and may they nest?
- **OPEN QUESTION:** Which escape sequences and raw-string forms exist?
- **OPEN QUESTION:** What syntax will hexadecimal, binary, exponent, and digit-separator
  forms use? Decimal numeric suffixes are already defined in `02-types.md`.
- **OPEN QUESTION:** Are newlines ever syntactically significant?
- **OPEN QUESTION:** Is a byte-order mark accepted at the beginning of a file?
