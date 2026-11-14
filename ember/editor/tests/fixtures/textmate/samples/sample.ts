// A small module: types, classes, generics, template strings, regex and comments.
import { readFileSync } from "fs";
import type { Options } from "./options";

/**
 * Counts words.
 * @param text the input
 */
export function countWords(text: string, { lower = true }: Options = {}): Map<string, number> {
  const counts = new Map<string, number>();
  for (const word of text.split(/\s+/)) {
    if (!word) continue;
    const key = lower ? word.toLowerCase() : word;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return counts;
}

interface Shape<T extends number = number> {
  readonly name: string;
  area(): T;
}

enum Color { Red = 1, Green, Blue }

abstract class Base implements Shape {
  protected static count = 0;
  constructor(public readonly name: string, private size: number = 10) {
    Base.count++;
  }
  abstract area(): number;
  get label(): string {
    return `${this.name} (${this.size}px) #${Base.count}`;
  }
}

class Square extends Base {
  area() { return this.size ** 2; }
}

const pattern = /^(?<year>\d{4})-(\d{2})-(\d{2})$/u;
const tagged = String.raw`line one
line two ${1 + 2}`;
let value: number | undefined = undefined;
type Pair<A, B> = [A, B];
const fn = async <T,>(x: T): Promise<T> => { await null; return x; };
/* block comment
   spanning lines */
label: for (let i = 0; i < 3; i++) { if (i === 1) break label; }
const greeting = "안녕 😀 world"; // 한글 주석 😀 ok
const tpl = `이모지 ${"😀"} 끝`;
