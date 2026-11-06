# Grammar fixtures

The `.tmLanguage.json` files here are copies of the grammars Code-OSS 1.139.1 ships in its built-in
extensions. The tests tokenize with them (no grammar is part of the product: the editor reads the
grammars of the extensions an OSE server has installed). Each is MIT licensed by its upstream project;
the file's own `version` field names the upstream commit.

| file | upstream | licence |
|---|---|---|
| TypeScript.tmLanguage.json, JavaScript.tmLanguage.json | github.com/microsoft/TypeScript-TmLanguage | MIT, Copyright (c) Microsoft Corporation |
| rust.tmLanguage.json | github.com/dustypomerleau/rust-syntax | MIT |
| MagicPython.tmLanguage.json, MagicRegExp.tmLanguage.json | github.com/MagicStack/MagicPython | MIT |
| JSON.tmLanguage.json | github.com/microsoft/vscode-JSON.tmLanguage | MIT, Copyright (c) Microsoft Corporation |
| markdown.tmLanguage.json | github.com/microsoft/vscode-markdown-tm-grammar | MIT |

The licences were read from the upstream repositories (GitHub's licence API reports MIT for four;
the JSON grammar's repository holds the Microsoft MIT text and GitHub does not classify its format).

MIT License: permission is granted, free of charge, to use, copy, modify, merge, publish, distribute,
sublicense and sell copies of the software and its documentation files, provided the copyright notice
and this permission notice are included in all copies or substantial portions. The software is provided
"as is", without warranty of any kind.
