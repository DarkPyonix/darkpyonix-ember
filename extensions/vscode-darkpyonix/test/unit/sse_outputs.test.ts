import { describe, expect, it } from "vitest";
import { SseParser, toKernelEvent } from "../../src/manager/sse";
import { ERROR_MIME, STDERR_MIME, STDOUT_MIME, convertOutput, decodeText } from "../../src/run/outputs";
import { mapPath } from "../../src/manager/discovery";

describe("SseParser", () => {
  it("parses id/event/data across chunk boundaries and CRLF", () => {
    const p = new SseParser();
    expect(p.push("id: 1043\nevent: out")).toEqual([]);
    const a = p.push('put\ndata: {"run_id":"r","index":3}\n\n: comment\n\nid: 7\r\nevent: x\r\ndata: a\r\ndata: b\r\n\r');
    expect(a).toEqual([{ id: "1043", event: "output", data: '{"run_id":"r","index":3}' }]);
    expect(p.push("\n")).toEqual([{ id: "7", event: "x", data: "a\nb" }]);
  });

  it("messages without id (replay_truncated) carry no seq", () => {
    const p = new SseParser();
    const [m] = p.push('event: replay_truncated\ndata: {"oldest_seq":5}\n\n');
    expect(toKernelEvent(m)).toEqual({ seq: null, type: "replay_truncated", data: { oldest_seq: 5 } });
  });
});

describe("convertOutput", () => {
  it("maps streams, rich data, images and errors", () => {
    expect(convertOutput({ output_type: "stream", name: "stdout", text: ["a", "b"] })).toMatchObject({ stream: "stdout" });
    expect(decodeText(convertOutput({ output_type: "stream", name: "stdout", text: ["a", "b"] }).items[0])).toBe("ab");
    expect(convertOutput({ output_type: "stream", name: "stderr", text: "e" }).items[0].mime).toBe(STDERR_MIME);
    expect(convertOutput({ output_type: "stream", name: "stdout", text: "" }).items[0].mime).toBe(STDOUT_MIME);

    const r = convertOutput({
      output_type: "execute_result", execution_count: 4, metadata: {},
      data: { "text/plain": ["4", "2"], "image/png": "iVBORw0KGgo=\n", "application/json": { a: 1 }, "text/html": "<b>x</b>" },
    });
    expect(r.items.map((i) => i.mime)).toEqual(["text/html", "image/png", "application/json", "text/plain"]);
    expect(decodeText(r.items[3])).toBe("42");
    expect(Array.from(r.items[1].data.slice(0, 4))).toEqual([0x89, 0x50, 0x4e, 0x47]);
    expect(JSON.parse(decodeText(r.items[2]))).toEqual({ a: 1 });
    expect(r.metadata.executionCount).toBe(4);

    const e = convertOutput({ output_type: "error", ename: "ValueError", evalue: "bad", traceback: ["l1", "l2"] });
    expect(e.items[0].mime).toBe(ERROR_MIME);
    expect(JSON.parse(decodeText(e.items[0]))).toEqual({ name: "ValueError", message: "bad", stack: "l1\nl2" });
  });
});

describe("mapPath", () => {
  it("uses the longest matching prefix", () => {
    const map = { "/Users/me": "/home/me", "/Users/me/proj": "/srv/proj" };
    expect(mapPath("/Users/me/proj/a.pynb", map)).toBe("/srv/proj/a.pynb");
    expect(mapPath("/Users/me/b.py", map)).toBe("/home/me/b.py");
    expect(mapPath("/tmp/c.py", map)).toBe("/tmp/c.py");
  });
});
