# Grammar fixtures

The `.tmLanguage.json` files in this folder are copies of the grammars that Code-OSS 1.139.1 ships in its
built-in extensions, taken unchanged. The tests tokenize with them. No grammar is part of the product: the
editor reads the grammars of the extensions an OSE server has installed. This folder is test data only and is
kept out of every package and release (see `ember/editor/Cargo.toml`, `exclude`).

Each file names its upstream in its own `information_for_contributors` and `version` fields; the table
repeats them. The licence of every upstream is MIT (read from the upstream repository's LICENSE file); the
copyright holders are listed below the table, each with the licence text.

| file here | upstream repository | upstream path | upstream commit | copyright |
|---|---|---|---|---|
| TypeScript.tmLanguage.json | github.com/microsoft/TypeScript-TmLanguage | TypeScript.tmLanguage | 48f608692aa6d6ad7bd65b478187906c798234a8 | Microsoft Corporation |
| JavaScript.tmLanguage.json | github.com/microsoft/TypeScript-TmLanguage | TypeScriptReact.tmLanguage | 48f608692aa6d6ad7bd65b478187906c798234a8 | Microsoft Corporation |
| JSON.tmLanguage.json | github.com/microsoft/vscode-JSON.tmLanguage | JSON.tmLanguage | 9bd83f1c252b375e957203f21793316203f61f70 | Microsoft Corporation |
| rust.tmLanguage.json | github.com/dustypomerleau/rust-syntax | syntaxes/rust.tmLanguage.json | ca34cf382a7b250144f2ccb99ff9af96912c79f2 | Dustin Pomerleau |
| MagicPython.tmLanguage.json | github.com/MagicStack/MagicPython | grammars/MagicPython.tmLanguage | 7d0f2b22a5ad8fccbd7341bc7b7a715169283044 | MagicStack Inc. |
| MagicRegExp.tmLanguage.json | github.com/MagicStack/MagicPython | grammars/MagicRegExp.tmLanguage | c9b3409deb69acec31bbf7913830e93a046b30cc | MagicStack Inc. |
| markdown.tmLanguage.json | github.com/microsoft/vscode-markdown-tm-grammar | syntaxes/markdown.tmLanguage | 0812fc4b190efc17bfed0d5b4ff918eff8e4e377 | Microsoft |

Two more files are Code-OSS's own and carry no upstream repository of their own: `jsdoc.ts.injection.tmLanguage.json`
and `jsdoc.js.injection.tmLanguage.json` come from `extensions/typescript-basics/syntaxes/` of github.com/microsoft/vscode
(Code-OSS 1.139.1), MIT, Copyright (c) Microsoft Corporation (the text above). `injection-host.tmLanguage.json` and
`injection-guest.tmLanguage.json` were written for these tests and are part of this repository.

(The upstream file is the plist or YAML original; Code-OSS converted each to JSON, which is the file here.
`JavaScript.tmLanguage.json` is the conversion of TypeScript-TmLanguage's `TypeScriptReact.tmLanguage`.)

## Microsoft Corporation (TypeScript-TmLanguage, vscode-JSON.tmLanguage)

MIT License

Copyright (c) Microsoft Corporation
All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the
following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO
EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
USE OR OTHER DEALINGS IN THE SOFTWARE.

## Dustin Pomerleau (rust-syntax)

MIT License

Copyright (c) 2020 Dustin Pomerleau

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the
following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO
EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
USE OR OTHER DEALINGS IN THE SOFTWARE.

## MagicStack Inc. (MagicPython)

The MIT License

Copyright (c) 2015-present MagicStack Inc.  http://magic.io

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the
following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO
EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
USE OR OTHER DEALINGS IN THE SOFTWARE.

## Microsoft (vscode-markdown-tm-grammar)

The MIT License (MIT)

Copyright (c) Microsoft 2018

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the
following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO
EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
USE OR OTHER DEALINGS IN THE SOFTWARE.

