// Refuse path IPC without grants (shared bridge).
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";

const ROOT = path.join(os.tmpdir(), `unpack-ipc-gates-${process.pid}-${Date.now()}`);
fs.mkdirSync(ROOT, { recursive: true });
const arch = path.join(ROOT, "secret.7z");
fs.copyFileSync(
  process.env.TESTDATA ? process.env.TESTDATA + "/plain.7z" : "/workspace/testdata/plain.7z",
  arch
);
const iso = path.join(ROOT, "secret.iso");
fs.writeFileSync(iso, Buffer.alloc(1024));

const shim = fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8");
const b = await puppeteer.launch({
  executablePath: "/usr/bin/google-chrome",
  args: ["--no-sandbox"],
});
const p = await b.newPage();
await p.evaluateOnNewDocument(shim);
await p.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" });
await new Promise((r) => setTimeout(r, 500));

const probes = [
  ["file_info", { path: arch }],
  ["list_archive", { path: arch }],
  ["read_entry", { path: arch, entry: "a.txt" }],
  ["read_file", { path: arch }],
  ["extract_entry", { id: "t1", path: arch, entry: "a.txt", out_file: path.join(ROOT, "out.bin") }],
  ["iso_info", { path: iso }],
  ["god_verify_only", { path: ROOT }],
  ["list_archives_in_folder", { path: ROOT }],
  ["preflight_local", { dest: ROOT, paths: [arch] }],
  ["save_text_file", { path: path.join(ROOT, "x.txt"), text: "hi" }],
];

const refused = await p.evaluate(async (list) => {
  const out = [];
  for (const [cmd, args] of list) {
    let err = false;
    try {
      await window.__TAURI__.core.invoke(cmd, args);
    } catch {
      err = true;
    }
    out.push({ cmd, err });
  }
  return out;
}, probes);

await b.close();
fs.rmSync(ROOT, { recursive: true, force: true });

let fails = 0;
for (const { cmd, err } of refused) {
  if (!err) {
    console.error("FAIL:", cmd, "should refuse ungranted path");
    fails++;
  } else {
    console.log("PASS", cmd, "refused");
  }
}
if (fails) process.exit(1);
console.log(`ipc-gates: ${probes.length}/${probes.length} refused`);
