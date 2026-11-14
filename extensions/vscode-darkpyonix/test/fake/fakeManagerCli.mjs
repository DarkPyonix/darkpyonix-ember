// A stand-in `darkpyonix` CLI for discovery tests: `manager --ephemeral` serves /health and,
// like the Rust manager (darkpyonix-core FR-M3, FR-C1), prints `{url, token, pid}` as the first
// stdout line and its notices on stderr. It writes no file. `--fail` exits 3 after a stderr line.
import { createServer } from "node:http";

const args = process.argv.slice(2);
if (args.includes("--fail")) {
  console.error("boom: cannot start kernel backend");
  process.exit(3);
}
if (args[0] !== "manager" || args[1] !== "--ephemeral") process.exit(2);
const s = createServer((q, r) => {
  r.writeHead(200, { "Content-Type": "application/json" });
  r.end(JSON.stringify({ status: "ok", version: "fake", home: process.env.DARKPYONIX_HOME ?? null, inherited_url: process.env.DARKPYONIX_MANAGER_URL ?? null }));
});
s.listen(0, "127.0.0.1", () => {
  const url = "http://127.0.0.1:" + s.address().port;
  process.stdout.write(JSON.stringify({ url, token: "spawned-token", pid: process.pid }) + "\n");
  console.error("darkpyonix manager listening on " + url);
});
// The idle timeout of a real ephemeral manager.
setTimeout(() => process.exit(0), Number(process.env.FAKE_IDLE_MS || 20000));
