# vscode-darkpyonix-theme

DarkPyonix custom theme for VS Code.

Imported from [DarkPyonix/vscode-darkpyonix-theme](https://github.com/DarkPyonix/vscode-darkpyonix-theme)@`5c2dbd78959751dcfefdc7f8570f5f84bb1078ce`
("Feat: Establish base theme foundation"). The upstream history (4 commits) stays in that
repository; the files here were copied unchanged except for this README, `package.json`
(Dark variant registered, version 0.0.2, `license`) and the new Dark theme file. Like the
rest of this repository it is licensed under Apache-2.0 (`LICENSE`); the upstream copy was MIT.

## Themes

| Label | `uiTheme` | File |
|---|---|---|
| DarkPyonix Light | `vs` | `themes/DarkPyonix Light-color-theme.json` (upstream) |
| DarkPyonix Dark | `vs-dark` | `themes/DarkPyonix Dark-color-theme.json` (derived here) |

## Palette

| Role | Light | Dark |
|---|---|---|
| Editor surface (parchment) | `#F5EFE0` | `#17120E` |
| Side surfaces (sand) | `#E8DECA` | `#211A15` |
| Lines / hover (line) | `#D3C5AE` | `#2A221B` |
| Text (ink) | `#3E2E22` | `#EFE6D6` |
| Accent (ember) | `#C65F3C` | `#E07A55` |

The Dark variant maps every Light UI color through this table; text on ember fills stays
dark (`#17120E`) for contrast, selections use `#4A3A2C`, and token colors are lightened
versions of the Light token colors. The IntelliJ plugin in `../intellij-darkpyonix` uses the
same ember/sand colors for its cell decorations.

## Try it

Open this folder in VS Code and press F5 (Extension Development Host), then pick the theme
with "Preferences: Color Theme". Package with `npx @vscode/vsce package`.
