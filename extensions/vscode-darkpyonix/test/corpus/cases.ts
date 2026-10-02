// Round-trip cases shared by the parser and editor-view tests (from core tests/test_fr_f1_format.py).
export const BOM = String.fromCharCode(0xfeff);

export const ROUNDTRIP_CASES = [
  "",
  "x = 1",
  "x = 1\n",
  "import os\nprint(os.name)\n",
  "# %% [code]\nx = 1\n",
  "# %%\nx = 1",
  "import a\r\n\r\n# %% [code]\r\nx = 1\r\n\r\n# %% [markdown]\r\nm()\r\n",
  '# %% Load data [code]\n# @id: "c-3f2a"\n# @collapsed: true\nx = 1\n',
  "# %% just a title\nx = 1\n\n\n# %% [mystery-type]\ny = 2\n",
  '# %% [code]\n# @n: 3\n# @l: [1, 2]\n# @o: {"a": null}\n# @s: hello world\nz\n',
  "# %% [code]\n# %% [code]\n",
  "# %%   [code]   \t\n  x\n",
  BOM + "# %% [code]\nx = 1\n",
  BOM + "pre = 1\r\n# %% [code]\r\nx = 1",
];

