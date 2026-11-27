// Test-only providers for Ember's live editor tests (SPEC FR-E12, #32 phase A step 2). Each
// answer carries a fixed marker, so a test can tell it came from this extension through the
// extension host and Ember's bridge: hover, CodeLens and inline completion on plain text.
const vscode = require('vscode');

exports.activate = function (context) {
  const selector = { language: 'plaintext' };
  context.subscriptions.push(vscode.commands.registerCommand('emberProbe.noop', () => {}));
  context.subscriptions.push(vscode.languages.registerHoverProvider(selector, {
    provideHover(document, position) {
      const range = document.getWordRangeAtPosition(position);
      const word = range ? document.getText(range) : '';
      return new vscode.Hover(new vscode.MarkdownString('ember-probe hover: ' + word), range);
    },
  }));
  context.subscriptions.push(vscode.languages.registerCodeLensProvider(selector, {
    provideCodeLenses() {
      return [new vscode.CodeLens(new vscode.Range(0, 0, 0, 0), { title: 'ember-probe lens', command: 'emberProbe.noop' })];
    },
  }));
  context.subscriptions.push(vscode.languages.registerInlineCompletionItemProvider(selector, {
    provideInlineCompletionItems(document, position) {
      return [new vscode.InlineCompletionItem('ember-probe-ghost', new vscode.Range(position, position))];
    },
  }));
};

exports.deactivate = function () {};
