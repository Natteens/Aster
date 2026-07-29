# Developing the extension

This file is for people changing the extension itself. To simply use it, read
[README.md](README.md).

## Try changes with F5

1. Open this folder (`editors/vscode`) as the workspace in VS Code.
2. Press `F5`. The `Run ASTER Extension` launch configuration starts an
   **Extension Development Host** — a second VS Code window with your local, uncommitted
   version of the extension loaded.
3. In that window, open `test/syntax-sample.aster` to check highlighting and snippets.

F5 is only for development. Users install the packaged `.vsix` instead and never see the
Extension Development Host.

## Build the `.vsix` package

Packaging uses [`@vscode/vsce`](https://github.com/microsoft/vscode-vsce), the standard
VS Code extension packer, declared as the only dev dependency.

```sh
cd editors/vscode
npm ci             # installs the locked development dependency
npm run validate   # lists exactly which files the package will contain
npm run package    # writes aster-language-<version>.vsix into this folder
```

> On Windows PowerShell, call `npm.cmd` instead of `npm` if plain `npm` is not resolved.

`node_modules/` and generated `*.vsix` files are git-ignored; don't commit them.
The package filename uses the version in `package.json`. That version is synchronized from the Rust
workspace by the automatic release workflow; check it locally with `npm run version:check`.

## Validate the JSON files

Everything in the extension is declarative JSON:

```sh
python3 -m json.tool package.json > /dev/null
python3 -m json.tool language-configuration.json > /dev/null
python3 -m json.tool syntaxes/aster.tmLanguage.json > /dev/null
python3 -m json.tool snippets/aster.json > /dev/null
python3 -m json.tool themes/aster-night-color-theme.json > /dev/null
```

`npm run validate` additionally checks the extension manifest itself.
