# Aster for Visual Studio Code

This extension makes VS Code understand `.aster` files. Open one and you get syntax
highlighting, comment toggling with `Ctrl+/`, matching brackets with auto-closing, and a
set of snippets (`func`, `class`, `struct`, `interface`, `if`, `for`, `while`, `log`,
`logw`, `loge`).

## Installing

The extension is installed from a `.vsix` package file. If you have one (or built one —
see [DEVELOPMENT.md](DEVELOPMENT.md)), install it in either way:

- In VS Code: open the **Extensions** panel, click `...` (top-right), choose
  **Install from VSIX...**, and pick the file.
- From a terminal:

  ```sh
  code --install-extension "<generated-file>.vsix"
  ```

That's it. From then on, any `.aster` file you open in any window is recognized — no
special setup or extra windows.

## Colors and the Aster Night theme

By default the extension follows your current color theme. It also ships an optional dark
theme called **Aster Night**, tuned for Aster: blue and purple keywords, cyan primitive
types, turquoise type names, soft-yellow function calls (including `Log`, `Log.Warning`,
and `Log.Error`), soft-orange strings, and muted comments. Red never appears on valid
code — it is reserved for real problems, like an invalid escape sequence.

To try it: open the Command Palette, run `Preferences: Color Theme`, and select
`Aster Night`. Your theme is never changed automatically.

## What works, and what doesn't yet

Works today: recognition of `.aster` files, syntax coloring, comments, bracket pairs,
basic indentation, folding, and snippets.

Not available yet: semantic autocomplete, error squiggles while you type, rename, and
go-to-definition. These need a language server — the future **Aster Language Server**
(`aster-lsp`) — and will arrive with it rather than being imitated here.
