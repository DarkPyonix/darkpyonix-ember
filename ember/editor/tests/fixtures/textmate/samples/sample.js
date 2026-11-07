'use strict';
const path = require('path');

/**
 * Joins path parts.
 * @param {...string} parts the pieces
 * @returns {string} the path
 */
function join(...parts) {
  return parts.filter(Boolean).join(path.sep);
}

class Counter {
  #count = 0;
  static create() { return new Counter(); }
  increment() { this.#count += 1; return this; }
  get value() { return this.#count; }
}

const re = /ab+c/gi, s = 'it\'s "quoted"', t = `total: ${join('a', 'b')} and ${1 + 1}`;
const obj = { key: 1, 'other-key': [1, 2, 3], method() { return null; }, ...rest };
export default async function main(args) {
  try {
    const result = await fetch(args.url, { method: 'POST' });
    return result.ok ? await result.json() : undefined;
  } catch (error) {
    console.error(error.message);
  } finally {
    /* cleanup */
  }
}
const html = `<div class="a">${s}</div>`;
if (typeof window !== 'undefined' && window.document) { document.title = 0x1F + 1e3 + .5; }
