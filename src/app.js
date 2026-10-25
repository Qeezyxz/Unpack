import * as N from "./native.js";
import { planImport } from "./importcheck.js";
import { zipSync, unzipSync, strToU8, strFromU8, Unzip, UnzipInflate } from "./vendor/fflate/browser.js";

const app = document.getElementById("app");
const fileInput = Object.assign(document.createElement("input"), {
  type: "file",
  multiple: true,
  className: "sr-only",
  accept: ".zip,.rar,.7z,.tar,.tgz,.gz,.bz2,.xz,application/zip,application/x-7z-compressed,application/vnd.rar,application/x-rar-compressed",
});
document.body.appendChild(fileInput);

let jobs = [];
let activeId = null;
let query = "";
let selectedPath = null;
let dragDepth = 0;
let rarEngine = null;
let sevenZipEngine = null;
let passwordPrompt = null;
let destHandle = null;
let destName = "";
let collision = "number";
let autoWrite = true;
let skipJunk = true;
let runToken = 0;

const folderCapable = "showDirectoryPicker" in window;
const READ_CHUNK = 8 * 1024 * 1024;
const SAFE_LOAD = 1500 * 1024 * 1024;
const BULKY_SIZE = 48 * 1024 * 1024;
const uid = () => Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 8);
const archiveStem = (n) => n.replace(/\.(zip|rar|7z|tar|tgz|gz|bz2|xz)$/i, "") || "archive";
const sanitizePath = (raw) =>
  String(raw || "")
    .replace(/\\/g, "/")
    .split("/")
    .map((s) => s.trim())
    .filter((s) => s && s !== "." && s !== "..")
    .join("/");
const isArchive = (f) => {
  const n = f.name.toLowerCase();
  return /\.(zip|rar|7z|tar|tgz|gz|bz2|xz)$/.test(n) || /zip|rar|7z|tar|gzip/.test(f.type || "");
};
const isHidden = (p) => p.split("/").some((s) => s.startsWith(".") || s === "__MACOSX");
const isJunk = (p) => {
  const parts = String(p || "").split("/");
  const name = (parts.at(-1) || "").toLowerCase();
  return isHidden(p) || name === "thumbs.db" || name === "desktop.ini" || name === ".ds_store";
};
const isImage = (p) => /\.(png|jpe?g|gif|webp|svg|bmp|ico)$/i.test(p);
const isText = (p) =>
  /\.(txt|md|json|csv|xml|html|css|js|ts|tsx|jsx|yml|yaml|toml|ini|log|svg|nfo)$/i.test(p);
const isBulky = (path, size = 0) =>
  /\.(iso|img|vmdk|vhd|vhdx|wim|esd|dmg|toast)$/i.test(path) || size >= BULKY_SIZE;

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const u = ["B", "KB", "MB", "GB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), u.length - 1);
  const v = bytes / 1024 ** i;
  return (v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)) + " " + u[i];
}

let folderConfirm = null; // { folder, archives: string[] }
async function notifyDone(title, body) {
  if (!N.IS_DESKTOP || !N.settings.notify) return;
  try { await N.invoke("notify_done", { title, body }); } catch {}
}
async function setTaskbar(pct) {
  if (!N.IS_DESKTOP) return;
  try { await N.invoke("set_taskbar_progress", { pct: pct == null ? null : Math.round(pct) }); } catch {}
}
function toast(msg) {
  let el = document.querySelector(".toast");
  if (!el) {
    el = document.createElement("div");
    el.className = "toast";
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add("show");
  clearTimeout(toast._t);
  toast._t = setTimeout(() => el.classList.remove("show"), 2600);
}

// Dismissable notice, shown once (Rust deletes the notice file when it is read).
function showMigrationNotice(msg) {
  const el = document.createElement("div");
  el.id = "migration-notice";
  el.setAttribute("role", "status");
  el.style.cssText = "position:fixed;left:50%;bottom:24px;transform:translateX(-50%);max-width:640px;z-index:9999;padding:12px 16px;border-radius:8px;background:var(--panel,#222);color:var(--text,#eee);border:1px solid var(--accent,#5a8);box-shadow:0 4px 16px rgba(0,0,0,.4);display:flex;gap:12px;align-items:flex-start";
  const t = document.createElement("div");
  t.textContent = msg;
  const b = document.createElement("button");
  b.textContent = "OK";
  b.setAttribute("data-migration-ok", "");
  b.onclick = () => el.remove();
  el.append(t, b);
  document.body.appendChild(el);
}

// Red error banner: stays on top of every dialog until dismissed or the next job starts.
let lastErrorDetail = "";
async function showError(message) {
  const el = document.getElementById("job-error");
  if (!el) return;
  const text = String(message ?? "").replace(/^Error:\s*/, "").trim() || "Something went wrong.";
  el.querySelector("[data-error-text]").textContent = text;
  const hintEl = el.querySelector("[data-error-hint]");
  let hint = "";
  if (N.IS_DESKTOP) {
    try {
      const h = await N.invoke("error_help", { message: text });
      hint = h?.hint || "";
    } catch {}
  }
  if (hint) {
    hintEl.textContent = hint;
    hintEl.removeAttribute("hidden");
    lastErrorDetail = `${text}\n\n${hint}`;
  } else {
    hintEl.textContent = "";
    hintEl.setAttribute("hidden", "");
    lastErrorDetail = text;
  }
  el.classList.remove("hidden");
}
function clearError() {
  const el = document.getElementById("job-error");
  if (!el) return;
  el.classList.add("hidden");
  el.querySelector("[data-error-text]").textContent = "";
  const hintEl = el.querySelector("[data-error-hint]");
  if (hintEl) { hintEl.textContent = ""; hintEl.setAttribute("hidden", ""); }
  lastErrorDetail = "";
}
function bindErrorBanner() {
  const el = document.getElementById("job-error");
  if (!el) return;
  el.querySelector("[data-error-dismiss]").addEventListener("click", clearError);
  el.querySelector("[data-error-copy]").addEventListener("click", async (e) => {
    const text = lastErrorDetail || el.querySelector("[data-error-text]").textContent;
    const b = e.currentTarget;
    try {
      await navigator.clipboard.writeText(text);
      b.textContent = "Copied";
    } catch {
      const range = document.createRange();
      range.selectNodeContents(el.querySelector("[data-error-text]"));
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
      b.textContent = "Selected";
    }
    setTimeout(() => { b.textContent = "Copy details"; }, 1500);
  });
}
bindErrorBanner();

// Discord Rich Presence: only a coarse view name and a count-based label leave the page (never
// file names or paths); Rust builds the text. Sent only when it changes.
let presenceKey = "";
function presenceNow() {
  let view = "extract";
  if (settingsOpen) view = "settings";
  else if (xboxView && !xboxView.hidden) view = "xbox";
  else if (modsView) view = "mods";
  else if (libraryView) view = "library";
  else if (godPanel) view = "convert";
  else if (homebrewView) view = "homebrew";
  const saving = jobs.filter((j) => j.phase === "saving").length;
  const converting = godPanel ? godPanel.items.filter((i) => i.phase === "working").length : 0;
  const sending = xboxView ? xboxView.sends.filter((x) => x.running).length : 0;
  const bits = [];
  if (saving) bits.push(`Extracting ${saving} archive${saving === 1 ? "" : "s"}`);
  if (converting) bits.push(`Converting ${converting} ISO${converting === 1 ? "" : "s"}`);
  if (sending) bits.push(`Sending ${sending} item${sending === 1 ? "" : "s"}`);
  return { view, job: bits.join(" · ") || null };
}
function updatePresence() {
  if (!N.IS_DESKTOP || !N.settings.discordPresence) return;
  const p = presenceNow();
  const key = p.view + "|" + (p.job || "");
  if (key === presenceKey) return;
  presenceKey = key;
  N.invoke("discord_set_activity", { view: p.view, job: p.job }).catch(() => {});
}

function detectFormat(buf, name = "") {
  const n = name.toLowerCase();
  if (buf.length >= 4 && buf[0] === 0x50 && buf[1] === 0x4b) return "zip";
  if (buf.length >= 7 && buf[0] === 0x52 && buf[1] === 0x61 && buf[2] === 0x72 && buf[3] === 0x21) return "rar";
  if (buf.length >= 6 && buf[0] === 0x37 && buf[1] === 0x7a && buf[2] === 0xbc && buf[3] === 0xaf) return "7z";
  if (buf.length >= 5 && buf[0] === 0x1f && buf[1] === 0x8b) return "gz";
  if (n.endsWith(".zip")) return "zip";
  if (n.endsWith(".rar")) return "rar";
  if (n.endsWith(".7z")) return "7z";
  return n.replace(/^\./, "") || "archive";
}

function looksEncrypted(err) {
  const m = String(err?.message || err || "").toLowerCase();
  return /password|encrypt|passphrase|hdr_encrypted|unsupportedencryption|wrong password/.test(m);
}

// Several queued archives may need a password at once; show the dialogs one at a time.
let passwordChain = Promise.resolve();
async function askPassword(filename, wrong = false) {
  const turn = passwordChain.then(
    () =>
      new Promise((resolve) => {
        passwordPrompt = { filename, resolve, wrong };
        render();
        requestAnimationFrame(() => document.getElementById("pw")?.focus());
      })
  );
  passwordChain = turn.catch(() => {});
  return turn;
}

async function fetchBinary(url) {
  const res = await fetch(url, { cache: "force-cache" });
  if (!res.ok) throw new Error("Could not load the built-in extractor (" + res.status + "). Reinstall Unpack.");
  return res.arrayBuffer();
}

function copyBytes(src) {
  if (!src) return new Uint8Array();
  if (src instanceof ArrayBuffer) return new Uint8Array(src.slice(0));
  const view = src instanceof Uint8Array ? src : new Uint8Array(src.buffer, src.byteOffset || 0, src.byteLength || 0);
  const out = new Uint8Array(view.byteLength);
  out.set(view);
  return out;
}

function isNotReadable(err) {
  const m = String(err?.message || err || "");
  return err?.name === "NotReadableError" || /could not be read|permission problems that have occurred after a reference/i.test(m);
}

async function readSlice(file, start, end) {
  const slice = file.slice(start, end);
  try {
    return new Uint8Array(await slice.arrayBuffer());
  } catch {
    const buf = await new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(reader.result);
      reader.onerror = () => reject(reader.error || new Error("NOT_READABLE"));
      reader.readAsArrayBuffer(slice);
    });
    return new Uint8Array(buf);
  }
}

async function readFileBytes(file, onStatus) {
  if (file?.bytes instanceof Uint8Array) return file.bytes;
  const size = Number(file.size) || 0;
  if (size <= 0) return copyBytes(await file.arrayBuffer());
  if (size > SAFE_LOAD) {
    const err = new Error("ARCHIVE_TOO_LARGE");
    err.bytes = size;
    throw err;
  }
  const out = new Uint8Array(size);
  let offset = 0;
  while (offset < size) {
    const end = Math.min(offset + READ_CHUNK, size);
    onStatus?.("Reading archive… " + Math.round((offset / size) * 100) + "%");
    const part = await readSlice(file, offset, end);
    out.set(part, offset);
    offset = end;
    await new Promise((r) => setTimeout(r, 0));
  }
  return out;
}

// fflate cannot decrypt; it silently returns scrambled bytes. Detect the
// "encrypted" flag in the ZIP central directory so we can hand off to 7-Zip.
function zipIsEncrypted(buf) {
  try {
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    let eocd = -1;
    for (let i = buf.length - 22; i >= Math.max(0, buf.length - 65557); i--) {
      if (dv.getUint32(i, true) === 0x06054b50) { eocd = i; break; }
    }
    if (eocd < 0) return false;
    const count = dv.getUint16(eocd + 10, true);
    let off = dv.getUint32(eocd + 16, true);
    for (let n = 0; n < count && off + 46 <= buf.length; n++) {
      if (dv.getUint32(off, true) !== 0x02014b50) break;
      if (dv.getUint16(off + 8, true) & 1) return true;
      off += 46 + dv.getUint16(off + 28, true) + dv.getUint16(off + 30, true) + dv.getUint16(off + 32, true);
    }
  } catch {}
  return false;
}

function extractZipFast(buf, warnings = []) {
  const out = unzipSync(buf, {
    filter: (file) => !isBulky(file.name, file.originalSize || file.size || 0),
  });
  const files = [];
  for (const [path, data] of Object.entries(out)) {
    if (/[\\/]$/.test(path)) continue; // directory entry, not a file
    const clean = sanitizePath(path);
    if (!clean) continue;
    try {
      const bytes = copyBytes(data);
      files.push({ path: clean, size: bytes.byteLength, data: bytes });
    } catch (err) {
      warnings.push(clean + ": " + (err?.message || "could not copy this file"));
    }
  }
  return files;
}

function listZipBulky(buf) {
  const bulky = [];
  try {
    unzipSync(buf, {
      filter: (file) => {
        const path = sanitizePath(file.name);
        const size = file.originalSize || file.size || 0;
        if (path && !/[\\/]$/.test(file.name) && isBulky(path, size)) bulky.push({ path, size, data: null, deferred: true });
        return false;
      },
    });
  } catch {}
  return bulky;
}

async function extractZipStreaming(file, warnings = [], onStatus) {
  const files = [];
  const pending = [];
  const unzipper = new Unzip((entry) => {
    const path = sanitizePath(entry.name);
    if (!path || isHidden(path) || /[\\/]$/.test(entry.name)) return;
    const size = entry.originalSize || entry.size || 0;
    if (isBulky(path, size)) {
      files.push({ path, size, data: null, deferred: true });
      return;
    }
    pending.push(
      new Promise((resolve) => {
        const chunks = [];
        let total = 0;
        entry.ondata = (err, dat, final) => {
          if (err) {
            warnings.push(path + ": " + err.message);
            resolve();
            return;
          }
          chunks.push(dat);
          total += dat.byteLength;
          if (final) {
            const bytes = new Uint8Array(total);
            let o = 0;
            for (const c of chunks) {
              bytes.set(c, o);
              o += c.byteLength;
            }
            files.push({ path, size: bytes.byteLength, data: bytes });
            resolve();
          }
        };
        try {
          entry.start();
        } catch (err) {
          warnings.push(path + ": " + (err?.message || "skip"));
          resolve();
        }
      })
    );
  });
  unzipper.register(UnzipInflate);
  const size = Number(file.size) || 0;
  for (let off = 0; off < size; off += READ_CHUNK) {
    onStatus?.("Streaming ZIP… " + Math.round((off / size) * 100) + "%");
    const part = await readSlice(file, off, Math.min(off + READ_CHUNK, size));
    unzipper.push(part, off + READ_CHUNK >= size);
    await new Promise((r) => setTimeout(r, 0));
  }
  await Promise.all(pending);
  return files;
}

async function getRarExtractor(data, password) {
  const { createExtractorFromData } = await import("./vendor/unrar/unrar.bundle.js");
  if (!rarEngine) {
    rarEngine = await fetchBinary("./vendor/unrar/unrar.wasm");
  }
  return createExtractorFromData({
    wasmBinary: rarEngine,
    data,
    password: password || "",
  });
}

async function extractRar(buf, password, onStatus, warnings = []) {
  onStatus?.("Loading RAR engine…");
  const data = buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
  const lister = await getRarExtractor(data, password);
  const listed = lister.getFileList();
  const headers = [];
  for (const h of listed.fileHeaders || []) headers.push(h);
  const extractor = await getRarExtractor(data, password);
  onStatus?.("Extracting RAR…");
  const smallNames = headers
    .filter((h) => !h.flags?.directory && !h.dir && !isBulky(h.name, h.unpSize || h.size || 0))
    .map((h) => h.name);
  const extracted = extractor.extract(smallNames.length ? { files: smallNames } : {});
  const files = [];
  for (const entry of extracted.files) {
    const header = entry.fileHeader || {};
    if (header.flags?.directory || header.dir) continue;
    const path = sanitizePath(header.name || header.fileName || "");
    if (!path) continue;
    if (isBulky(path, header.unpSize || header.size || 0)) continue;
    try {
      const bytes = copyBytes(entry.extraction || []);
      files.push({ path, size: bytes.byteLength, data: bytes });
    } catch (err) {
      warnings.push(path + ": skipped (" + (err?.message || "unreadable") + ")");
    }
  }
  for (const h of headers) {
    if (h.flags?.directory || h.dir) continue;
    const path = sanitizePath(h.name);
    if (path && isBulky(path, h.unpSize || h.size || 0)) {
      files.push({ path, size: h.unpSize || h.size || 0, data: null, deferred: true });
    }
  }
  if (!files.length && headers.length && !password) throw new Error("password required");
  return files;
}

function rmVirtual(sz, path) {
  let names;
  try { names = sz.FS.readdir(path); } catch { return; }
  for (const name of names) {
    if (name === "." || name === "..") continue;
    const child = path.endsWith("/") ? path + name : path + "/" + name;
    let stat;
    try { stat = sz.FS.stat(child); } catch { continue; }
    if (sz.FS.isDir(stat.mode)) {
      rmVirtual(sz, child);
      try { sz.FS.rmdir(child); } catch {}
    } else {
      try { sz.FS.unlink(child); } catch {}
    }
  }
}

function walkVirtual(sz, path, prefix = "", warnings = []) {
  const files = [];
  let names;
  try { names = sz.FS.readdir(path); } catch { return files; }
  for (const name of names) {
    if (name === "." || name === "..") continue;
    const child = path.endsWith("/") ? path + name : path + "/" + name;
    const rel = prefix ? prefix + "/" + name : name;
    let stat;
    try { stat = sz.FS.stat(child); } catch (err) {
      warnings.push(rel + ": " + (err?.message || "could not stat"));
      continue;
    }
    if (sz.FS.isDir(stat.mode)) files.push(...walkVirtual(sz, child, rel, warnings));
    else {
      try {
        const data = sz.FS.readFile(child);
        const bytes = copyBytes(data);
        const clean = sanitizePath(rel);
        if (clean) files.push({ path: clean, size: bytes.byteLength, data: bytes });
      } catch (err) {
        warnings.push(rel + ": skipped (" + (err?.message || "unreadable") + ")");
      }
    }
  }
  return files;
}

async function getSevenZip(onStatus, logs) {
  const SevenZip = (await import("./vendor/7z-wasm/7zz.es6.js")).default;
  if (!sevenZipEngine) {
    onStatus?.("Loading 7Z engine…");
    sevenZipEngine = await fetchBinary("./vendor/7z-wasm/7zz.wasm");
  }
  onStatus?.("Starting 7Z engine…");
  const sz = await SevenZip({
    wasmBinary: sevenZipEngine,
    locateFile: (p) => new URL("./vendor/7z-wasm/" + p, location.href).href,
    noExitRuntime: true,
    // Never let 7-Zip ask for a password through window.prompt(); we pass -p ourselves.
    stdin: () => null,
    print: (s) => logs && logs.push(String(s)),
    printErr: (s) => logs && logs.push(String(s)),
  });
  try { sz.FS.mkdir("/in"); } catch {}
  try { sz.FS.mkdir("/out"); } catch {}
  return sz;
}

function parse7zList(logs) {
  const entries = [];
  let cur = {};
  for (const line of logs) {
    const text = String(line).trim();
    if (!text) {
      if (cur.path) entries.push(cur);
      cur = {};
      continue;
    }
    const path = text.match(/^Path = (.*)$/i);
    const size = text.match(/^Size = (\d+)$/i);
    const attr = text.match(/^Attributes = (.*)$/i);
    if (path) cur.path = path[1];
    if (size) cur.size = Number(size[1]);
    if (attr) cur.dir = /D/.test(attr[1]);
  }
  if (cur.path) entries.push(cur);
  return entries.filter((e) => e.path && e.path !== "/in/archive.7z" && e.path !== "archive.7z" && !e.dir);
}

async function extract7z(buf, password, onStatus, warnings = [], type = "7z") {
  const logs = [];
  const sz = await getSevenZip(onStatus, logs);
  onStatus?.("Listing 7Z…");
  rmVirtual(sz, "/in");
  rmVirtual(sz, "/out");
  const archiveName = "archive.7z";
  sz.FS.writeFile("/in/" + archiveName, buf);
  // A dummy password stops 7-Zip from prompting; a wrong one just reports "Wrong password".
  const pwArg = "-p" + (password || "\u0001no-password");
  try { sz.callMain(["l", "-slt", pwArg, "/in/" + archiveName]); } catch {}
  const listed = parse7zList(logs);
  onStatus?.("Extracting 7Z…");
  const args = ["x", "/in/" + archiveName, "-o/out", "-y", "-t" + type, "-x!*.iso", "-x!*.ISO", "-x!*.img", "-x!*.IMG"];
  args.push(pwArg);
  let code = 0;
  try {
    code = sz.callMain(args);
  } catch (err) {
    const status = err?.status ?? err?.message;
    if (typeof status === "number") code = status;
    else if (!/exit/i.test(String(err))) throw err;
  }
  const files = walkVirtual(sz, "/out", "", warnings);
  for (const entry of listed) {
    const path = sanitizePath(entry.path);
    if (path && isBulky(path, entry.size || 0) && !files.some((f) => f.path === path)) {
      files.push({ path, size: entry.size || 0, data: null, deferred: true });
    }
  }
  rmVirtual(sz, "/in");
  rmVirtual(sz, "/out");
  if (logs.some((l) => /wrong password|can ?not open encrypted/i.test(l))) throw new Error(password ? "wrong password" : "password required");
  if (!files.length) {
    if (password) throw new Error("password required or archive is empty");
    if (code && code !== 0) throw new Error("7Z extractor failed (code " + code + ")");
    throw new Error("password required");
  }
  return files;
}

async function extractArchive(job, password, onStatus) {
  const warnings = job.warnings || (job.warnings = []);
  const source = job.sourceFile || job.file;
  const size = Number(source?.size || job.size || 0);
  let format = detectFormat(new Uint8Array(), job.name);
  try {
    const head = await readSlice(source, 0, Math.min(16, size || 16));
    format = detectFormat(head, job.name);
  } catch {}
  job.format = format;

  if (format === "zip" && size > SAFE_LOAD) {
    onStatus?.("Streaming large ZIP…");
    return { files: await extractZipStreaming(source, warnings, onStatus), format };
  }

  onStatus?.("Reading archive…");
  const buf = job.bytes || await readFileBytes(source, onStatus);
  job.bytes = buf;
  format = detectFormat(buf, job.name);
  job.format = format;

  if (format === "zip" && zipIsEncrypted(buf)) {
    return { files: await extract7z(buf, password, onStatus, warnings, "zip"), format };
  }
  if (format === "zip") {
    onStatus?.("Extracting ZIP…");
    const files = extractZipFast(buf, warnings);
    files.push(...listZipBulky(buf));
    return { files, format };
  }
  if (format === "rar") {
    return { files: await extractRar(buf, password, onStatus, warnings), format };
  }
  if (format === "7z") {
    return { files: await extract7z(buf, password, onStatus, warnings), format };
  }
  throw new Error("Unsupported archive type. Use ZIP, RAR, or 7Z.");
}

// ---------------------------------------------------------------------------
// Desktop (native 7-Zip) jobs
// ---------------------------------------------------------------------------
let nativeOk = false;
let paintQueued = false;

function schedulePaint() {
  // Progress events can arrive many times a second; repaint at most once per frame
  // and only touch progress bars / status text so typing in the search box isn't disturbed.
  if (paintQueued) return;
  paintQueued = true;
  requestAnimationFrame(() => {
    paintQueued = false;
    for (const job of jobs) { paintJob(job); for (const g of job.gods || []) paintJob(g); }
  });
}

// R25 L3: a GOD sub-row shows "status — error/warning"; the per-frame repaint must produce the
// same text as render(), or the warning vanishes until the next full render.
function godStatusText(g) {
  return g.error ? g.status + " — " + g.error : g.warning ? g.status + " — " + g.warning : g.status || "";
}

function paintJob(job) {
  const sel = (attr) => document.querySelectorAll(`[${attr}="${CSS.escape(job.id)}"]`);
  sel("data-job-bar").forEach((el) => { el.style.width = (job.progress || 0) + "%"; });
  sel("data-job-status").forEach((el) => {
    el.textContent = el.dataset.statusKind === "god" ? godStatusText(job) : job.status || "";
  });
}

async function addPaths(paths, action = "open") {
  if (action === "convert-god") {
    const isos = [];
    for (const p of paths) {
      try {
        const info = await N.invoke("file_info", { path: p });
        if (!info.is_dir && /\.iso$/i.test(info.name)) isos.push(info);
      } catch (err) { toast(String(err)); }
    }
    if (isos.length) openGod(isos);
    else toast("No .iso files to convert.");
    return;
  }
  const archives = [];
  const others = [];
  const folders = [];
  for (const p of paths) {
    try {
      const info = await N.invoke("file_info", { path: p });
      if (info.is_dir) folders.push(info);
      else if (N.ARCHIVE_RE.test(info.name)) archives.push(info);
      else others.push(info);
    } catch (err) {
      toast(String(err));
    }
  }
  // Folder drop: if it only contains archives/ISOs (and no loose files), confirm adding them to the
  // extract queue. Mixed drops (folder + files) still open Create archive — that stays explicit.
  if (folders.length && !composer && !godPanel && !others.length) {
    const found = [];
    for (const f of folders) {
      try {
        // Session grant comes from Tauri DragDrop / __test_drop_grant — no page-callable grant_dir.
        const list = await N.invoke("list_archives_in_folder", { path: f.path });
        found.push(...list);
      } catch (err) { toast(String(err)); }
    }
    if (found.length && !archives.length) {
      folderConfirm = { folder: folders.map((f) => f.path).join(", "), archives: [...new Set(found)] };
      render();
      return;
    }
    if (!found.length && !archives.length) { toast("No archives or ISOs in that folder. Use Create archive to pack it."); return; }
  }
  if (godPanel) {
    const isos = others.filter((i) => !i.is_dir && /\.iso$/i.test(i.name));
    if (isos.length) openGod(isos);
    else toast("Only .iso files can be converted to GOD.");
    return;
  }
  // While the "Create archive" window is open, everything dropped is added to it.
  if (composer && !composer.busy) {
    openComposer([...others, ...archives, ...folders]);
    return;
  }
  if (others.length || folders.length) onNonArchives([...others, ...folders], archives.length);
  const opening = [];
  for (const info of archives) {
    const job = {
      id: uid(),
      native: true,
      autoAction: action,
      path: info.path,
      name: info.name,
      size: info.size,
      files: [],
      warnings: [],
      format: "",
      phase: "opening",
      status: "Queued",
      progress: 4,
      error: "",
      password: "",
      cancelled: false,
    };
    jobs = [job, ...jobs];
    activeId = job.id;
    selectedPath = null;
    query = "";
    render();
    opening.push(nativeOk ? openNative(job) : openViaWasm(job));
  }
  await Promise.all(opening);
}

// 7za missing (shouldn't happen in an installed copy): read the file and use the in-page engines.
async function openViaWasm(job) {
  try {
    job.status = "Reading archive…";
    render();
    const buf = await N.invoke("read_file", { path: job.path });
    const bytes = new Uint8Array(buf);
    job.native = false;
    job.sourceFile = new File([bytes], job.name);
    job.file = job.sourceFile;
    await runExtract(job);
  } catch (err) {
    job.phase = "failed";
    job.status = "Failed";
    job.error = friendlyError(err);
    render();
  }
}

async function askPasswordNative(job, wrong) {
  job.status = wrong ? "Wrong password — try again" : "Password required";
  job.progress = 0;
  render();
  const pw = await askPassword(job.name, wrong);
  if (!pw) {
    job.phase = "failed";
    job.status = "Skipped";
    job.error = "Skipped — password not entered. The rest of the queue continues.";
    render();
    return null;
  }
  job.password = pw;
  return pw;
}

async function openNative(job) {
  job.phase = "opening";
  job.status = job.password ? "Unlocking archive…" : "Reading contents…";
  job.progress = 18;
  job.error = "";
  render();
  try {
    const listing = await N.invoke("list_archive", { path: job.path, password: job.password || null });
    job.format = listing.format;
    job.encrypted = listing.encrypted;
    job.files = listing.entries
      .filter((e) => !e.dir)
      .map((e) => ({ path: e.path, size: e.size, data: null, native: true, encrypted: e.encrypted }));
    if (!job.files.length) {
      job.phase = "failed";
      job.error = "The archive opened, but no files were found.";
      render();
      return;
    }
    // ZIPs (and 7z without hidden names) list fine without the password; ask now
    // so extraction doesn't stop half-way.
    if (listing.encrypted && !job.password) {
      if (!(await askPasswordNative(job))) return;
    }
    job.phase = "ready";
    job.progress = 100;
    job.status = `${job.files.length} files · ${formatBytes(job.files.reduce((n, f) => n + f.size, 0))}`;
    render();
    // Explorer "Extract here" / "Extract to folder" always go next to the archive.
    const auto = job.autoAction;
    job.autoAction = "open";
    if (auto === "extract-here") return enqueue(job, { layout: "here", mode: "beside" });
    if (auto === "extract-folder") return enqueue(job, { layout: "folder", mode: "beside" });
    // Like the web version: auto-extract only once the user has chosen a destination folder.
    if (autoWrite && settings().destMode === "folder" && settings().destPath) enqueue(job, {});
  } catch (err) {
    const code = N.errCode(err);
    if (code === "PASSWORD_REQUIRED" || code === "WRONG_PASSWORD") {
      if (await askPasswordNative(job, code === "WRONG_PASSWORD")) return openNative(job);
      return;
    }
    job.phase = "failed";
    job.status = "Failed";
    job.progress = 0;
    job.error = friendlyError(err);
    showError(`${job.name}: ${job.error}`);
    render();
  }
}

function settings() {
  return N.settings;
}

/** Work out the destination folder for a job. Returns null if the user cancelled. */
async function resolveDest(job, mode) {
  const m = mode || settings().destMode;
  if (m === "beside") return N.dirName(job.path);
  if (m === "folder" && settings().destPath) return settings().destPath;
  const picked = await N.pickFolder();
  return picked || null;
}

/**
 * Extract a native job. opts.layout: "folder" (default) | "here"; opts.mode: override dest mode.
 */
async function preflightDest(dest, paths, autoIso = "off") {
  if (!N.IS_DESKTOP || !dest || !paths?.length) return true;
  try {
    const r = await N.invoke("preflight_local", { dest, paths, autoIso });
    const blocks = (r.issues || []).filter((i) => i.level === "block");
    if (blocks.length) {
      showError(blocks.map((i) => i.text).join(" "));
      return false;
    }
    const warns = (r.issues || []).filter((i) => i.level === "warn");
    if (warns.length) toast(warns[0].text);
  } catch {}
  return true;
}

async function extractNative(job, opts = {}) {
  if (job.phase === "saving") return;
  const layout = opts.layout || "folder";
  const dest = opts.dest || (await resolveDest(job, opts.mode));
  if (!dest) return;
  if (!(await preflightDest(dest, [job.path], N.settings.autoIsoMode))) {
    job.phase = "failed";
    job.status = "Blocked";
    onJobDone?.(job, layout, opts);
    render();
    return;
  }
  job.cancelled = false;
  job.phase = "saving";
  job.progress = 0;
  job.error = "";
  job.status = "Extracting…";
  clearError();
  render();
  const off = N.onJobProgress(job.id, (p) => {
    job.progress = p.percent;
    job.status = `Extracting… ${p.percent}%` + (p.file ? " · " + p.file : "");
    schedulePaint();
  });
  try {
    const res = await N.invoke("extract_archive", {
      id: job.id,
      path: job.path,
      dest,
      password: job.password || null,
      collision,
      layout,
      skipJunk,
      sealIsos: N.settings.autoIsoMode !== "off" && N.settings.deleteIsoAfterGod,
      nested: N.settings.nested,
      autoIso: N.settings.autoIsoMode,
    });
    job.output = res.output;
    job.warnings = [...(res.warnings || [])];
    const inside = nestedNote(res);
    if (inside.warn) job.warnings.push(inside.warn);
    job.progress = 100;
    if (res.skipped) {
      job.phase = "skipped";
      job.status = "Already in this folder — skipped";
    } else {
      job.phase = "saved";
      job.status = `Saved ${res.files} files to ${res.output}${inside.status}`;
    }
    onJobDone?.(job, layout, opts);
    if (job.phase === "saved" && N.settings.autoIsoMode !== "off") queueExtractedIsos(job, layout, res);
    if (job.phase === "saved" && layout !== "here" && res.dlcs?.length) queueDlcFromExtract(job, res.dlcs, [N.dirName(res.output)]);
  } catch (err) {
    const code = N.errCode(err);
    if (code === "PASSWORD_REQUIRED" || code === "WRONG_PASSWORD") {
      off();
      if (await askPasswordNative(job, code === "WRONG_PASSWORD" || !!job.password)) return extractNative(job, { ...opts, dest });
      return;
    }
    job.progress = 0;
    if (code === "CANCELLED") {
      job.phase = "failed";
      job.status = "Cancelled";
      job.error = "";
    } else {
      job.phase = "failed";
      job.status = "Failed";
      job.error = friendlyError(err);
      showError(`${job.name}: ${job.error}`);
    }
    onJobDone?.(job, layout, opts);
  } finally {
    off();
    render();
  }
}
let onJobDone = null; // set by the history section below

/** Wording for "zips inside zips": how many were opened, and how many were left closed and why. */
function nestedNote(res) {
  const opened = res.nested || 0, left = res.inner_left || 0;
  const status = opened ? ` · opened ${opened} archive${opened === 1 ? "" : "s"} inside` : "";
  const plural = left === 1 ? "archive was" : "archives were";
  const warn = !left ? "" : N.settings.nested
    ? `${left} ${plural} inside and left closed (password, damaged, or too many levels). They are in the output folder.`
    : `${left} ${plural} inside and left closed. Turn on "Open zips inside zips" in Settings to open them.`;
  return { status, warn };
}

// Batch "Auto-convert ISOs to GOD": after an archive extracts into its own new folder, every
// .iso inside it is converted (one at a time, app-wide) into "<Game> (GOD)" next to that folder,
// i.e. in the chosen output folder. Non-Xbox ISOs are skipped and kept.
let godChain = Promise.resolve();
/** Archive names still waiting or extracting (for the missing-disc check). */
const extractingNames = () => jobs.filter((j) => ["ready", "queued", "opening", "saving"].includes(j.phase)).map((j) => j.name || N.baseName(j.path || ""));
/** Where every disc of this ISO's game is; null on error (never blocks converting). */
const discStatus = (path, queued, dest) => N.invoke("iso_disc_status", { path, queued, extracting: extractingNames(), dest: dest || null }).catch(() => null);
const missingText = (st) => `Disc ${st.missing.join(", ")} of ${st.count} of this game is missing. You can still convert; add the missing disc later and it joins the same game folder.`;
async function queueExtractedIsos(job, layout, res) {
  const mode = N.settings.autoIsoMode; // off | god | xex
  if (mode === "off") return;
  const label = mode === "xex" ? "extracted folder" : "GOD";
  const cmd = mode === "xex" ? "xex_extracted" : "god_extracted";
  if (layout === "here") {
    (job.warnings ||= []).push(`Auto ISO → ${label} only runs when each archive gets its own folder (not with Extract here).`);
    return render();
  }
  const isos = res.isos || [];
  if (!isos.length) return;
  const wantDelete = N.settings.deleteIsoAfterGod;
  const tokens = res.iso_tokens || [];
  const gods = isos.map((p, i) => ({ id: uid(), token: tokens[i] || "", path: p, name: N.baseName(p), phase: tokens[i] ? "queued" : "skipped", status: tokens[i] ? `Waiting to convert to ${label}…` : "Skipped: couldn't read this ISO after extracting · ISO kept", progress: 0, error: "", mode, disc: 1 }));
  // Multi-disc: real disc numbers (header, then name), disc 1 before disc 2 of the same game.
  const others = () => jobs.flatMap((j) => j.gods || []).filter((x) => x.phase === "queued").map((x) => x.path);
  for (const g of gods) {
    if (g.phase !== "queued") continue;
    const st = await discStatus(g.path, [...others(), ...gods.map((x) => x.path)]);
    if (!st) continue;
    g.disc = st.disc || 1;
    g.titleId = st.title_id || "";
    g.discLine = st.line;
    if (st.missing.length) {
      g.discWarn = missingText(st);
      (job.warnings ||= []).push(`${g.name}: ${g.discWarn}`);
      batchStats.discWarn.push(`${g.name}: ${st.line}`);
    }
    // Discs of this game next to it but not in this batch: offer / add per the companion setting.
    const extra = st.slots.filter((x) => x.state === "next_to_it" && x.path && !isos.includes(x.path)).map((x) => x.path);
    if (extra.length && N.settings.companionDiscMode !== "never") {
      const infos = [];
      for (const p of extra) { const fi = await N.invoke("file_info", { path: p }).catch(() => null); if (fi) infos.push(fi); }
      if (infos.length) {
        if (N.settings.companionDiscMode === "always") openGod(infos);
        else { openGod([]); godPanel.offer = [...new Set([...(godPanel.offer || []), ...extra])]; }
      }
    }
  }
  gods.sort((a, b) => (a.titleId || a.path).localeCompare(b.titleId || b.path) || a.disc - b.disc);
  job.gods = gods;
  render();
  for (const g of job.gods) {
    const run = async () => {
      if (g.phase !== "queued") return;
      // A lower disc of the same game is still queued or extracting: let it go first (once it
      // is done or fails, this disc runs), so disc 1 owns the game folder and default.xex.
      if (g.disc > 1 && !g.waited) {
        const st = await discStatus(g.path, jobs.flatMap((j) => j.gods || []).filter((x) => x !== g && x.phase === "queued").map((x) => x.path));
        if (st && st.slots.some((x) => x.disc < g.disc && (x.state === "queued" || x.state === "extract_queue"))) {
          g.status = `Waiting for disc ${st.slots.find((x) => x.disc < g.disc && x.state !== "missing" && x.state !== "converted").disc} of this game…`;
          render();
          await new Promise((r) => setTimeout(r, 400));
          if (!st.slots.some((x) => x.disc < g.disc && x.state === "extract_queue") || (g.tries = (g.tries || 0) + 1) > 1500) g.waited = true;
          godChain = godChain.then(run);
          return;
        }
      }
      g.phase = "saving";
      g.status = `Converting to ${label}…`;
      if (wantDelete) g.size = (await N.invoke("file_info", { path: g.path }).catch(() => ({}))).size || 0;
      render();
      const off = N.onJobProgress(g.id, (p) => { g.progress = p.percent; g.status = `${label} ${p.percent}% · ${p.file}`; schedulePaint(); });
      try {
        const r = await N.invoke(cmd, { id: g.id, token: g.token, deleteIso: wantDelete });
        g.phase = "saved";
        g.progress = 100;
        g.status = `${label} done → ${r.output}${discNote(r)}${r.iso_deleted ? " · extracted ISO deleted" : r.kept_reason ? " · " + r.kept_reason : ""}`;
        if (r.warning) g.warning = r.warning;
        addHistory({ name: r.name || g.name, source: g.path, output: r.output, result: "created", label: mode === "xex" ? "XEX auto-extracted" : "GOD auto-converted" });
        retryDlcNotFound();
        batchStats.godOk++;
        if (r.iso_deleted) batchStats.freed += g.size || 0;
      } catch (err) {
        const code = N.errCode(err);
        g.progress = 0;
        batchStats[/Not an Xbox|executable|CANCELLED/i.test(code) ? "godSkip" : "godFail"]++;
        const raw = String(err?.message || err || "");
        if (code === "CANCELLED") { g.phase = "skipped"; g.status = `${label} cancelled · ISO kept`; }
        else if (/^Skipped: this disc/.test(raw)) { g.phase = "skipped"; g.status = `${raw} · ISO kept`; }
        else if (/truncated/i.test(code)) { g.phase = "failed"; g.status = "Skipped: ISO is incomplete/truncated · ISO kept"; g.error = friendlyError(err); }
        else if (/Not an Xbox ISO|executable/i.test(code)) { g.phase = "skipped"; g.status = "Skipped: not an Xbox 360 / Xbox ISO · ISO kept"; }
        else { g.phase = "failed"; g.status = `${label} failed · ISO kept`; g.error = friendlyError(err); showError(`${g.name}: ${g.error}`); }
        addHistory({ name: g.name, source: g.path, output: "", result: g.phase === "failed" ? "failed" : "skipped", label: g.phase === "failed" ? `${label} failed` : `${label} skipped` });
      } finally {
        off();
        render();
      }
    };
    godChain = godChain.then(run);
  }
}
// ---- Batch summary (shown when everything in the batch, incl. GOD + DLC, is finished) ----
const newStats = () => ({ extracted: 0, skipped: 0, failed: 0, godOk: 0, godSkip: 0, godFail: 0, dlcOk: 0, dlcMissing: 0, dlcOther: 0, dlcFail: 0, freed: 0, lastOutput: "", failedJobs: [], skippedJobs: [], discWarn: [] });
let batchStats = newStats();
let summary = null;
let summaryTimer = null;
function scheduleSummary() {
  clearTimeout(summaryTimer);
  summaryTimer = setTimeout(async () => {
    await godChain;
    if (jobs.some((j) => j.phase === "saving" || j.phase === "queued" || j.phase === "opening")) return scheduleSummary();
    const st = batchStats;
    setTaskbar(null);
    const parts = [];
    if (st.extracted) parts.push(`${st.extracted} extracted`);
    if (st.godOk) parts.push(`${st.godOk} GOD`);
    if (st.dlcOk) parts.push(`${st.dlcOk} DLC/TU`);
    if (st.failed) parts.push(`${st.failed} failed`);
    notifyDone("Unpack — batch finished", parts.join(" · ") || "Done");
    if (st.extracted + st.failed + st.skipped > 1 || st.godOk + st.godFail + st.dlcOk > 0) { summary = { ...st, failedJobs: [...(st.failedJobs || [])], skippedJobs: [...(st.skippedJobs || [])], discWarn: [...(st.discWarn || [])] }; render(); }
    batchStats = newStats();
  }, 600);
}
function renderSummary() {
  if (!summary) return "";
  const s = summary;
  const cell = (n, label, cls = "") => `<div class="sum-cell ${cls}"><b>${n}</b><span>${label}</span></div>`;
  const retry = (s.failedJobs || []).length ? `<button class="btn btn-secondary btn-sm" data-sum-retry type="button">Retry failed (${s.failedJobs.length})</button>` : "";
  return `<section class="summary" role="status" aria-live="polite">
    <div class="sum-art">${SUMMARY_ART}</div>
    <div class="sum-body"><strong>Batch finished</strong>
      <div class="sum-grid">${cell(s.extracted, "extracted", "ok")}${s.failed ? cell(s.failed, "failed", "bad") : ""}${s.skipped ? cell(s.skipped, "skipped") : ""}
      ${s.godOk + s.godSkip + s.godFail ? cell(s.godOk, "GOD converted", "ok") + (s.godSkip ? cell(s.godSkip, "ISOs skipped") : "") + (s.godFail ? cell(s.godFail, "GOD failed", "bad") : "") : ""}
      ${s.dlcOk + s.dlcMissing + s.dlcFail ? cell(s.dlcOk, "DLC / TU installed", "ok") + (s.dlcMissing ? cell(s.dlcMissing, "game not found") : "") + (s.dlcFail ? cell(s.dlcFail, "DLC failed", "bad") : "") : ""}
      ${s.freed ? cell(formatBytes(s.freed), "space freed", "ok") : ""}</div>
      ${(s.discWarn || []).length ? `<div class="warn-text" data-sum-discs>⚠ Missing discs: ${s.discWarn.map((w) => escapeHtml(w)).join("<br>")}</div>` : ""}</div>
    <div class="actions">${retry}${s.lastOutput ? `<button class="btn btn-secondary btn-sm" data-sum-open type="button" title="Show the output in Explorer">Open folder</button>` : ""}<button class="btn btn-quiet" data-sum-close type="button" aria-label="Dismiss summary">${SV(14, 14, ICONS.x)}</button></div>
  </section>`;
}
async function retryFailedBatch() {
  const list = summary?.failedJobs || [];
  if (!list.length) return;
  summary = null;
  for (const ref of list) {
    const job = jobs.find((j) => j.id === ref.id);
    if (!job || job.phase !== "failed") continue;
    job.phase = "ready";
    job.status = "Queued";
    job.error = "";
    job.progress = 0;
    await enqueue(job, ref.opts || {});
  }
  render();
}
const SUMMARY_ART = `<svg viewBox="0 0 64 64" width="56" height="56" aria-hidden="true"><defs><linearGradient id="sg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="var(--accent)"/><stop offset="1" stop-color="var(--accent2)"/></linearGradient></defs><circle cx="32" cy="32" r="28" fill="none" stroke="url(#sg)" stroke-width="3" class="sum-ring"/><path d="M20 33l8 8 16-17" fill="none" stroke="url(#sg)" stroke-width="4" stroke-linecap="round" stroke-linejoin="round" class="sum-check"/></svg>`;

// ---- Library: games found in the library folders, with DLC/TU counts and Copy to USB ----
let libraryView = null; // { loading, games, copying: {path: {...}} }
function tileColor(tid) {
  let h = 0;
  for (const c of tid) h = (h * 31 + c.charCodeAt(0)) % 360;
  return [h, (h + 50) % 360];
}
function gameTile(g, coverUrl) {
  if (coverUrl) return `<img class="lib-cover" src="${escapeHtml(coverUrl)}" alt="" loading="lazy"/>`;
  const [a, b] = tileColor(g.title_id);
  const initials = g.name.replace(/[^\p{L}\p{N} ]/gu, "").split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join("").toUpperCase() || "?";
  return `<svg class="tile" viewBox="0 0 64 80" aria-hidden="true"><defs><linearGradient id="t${escapeHtml(g.title_id)}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${a} 70% 55%)"/><stop offset="1" stop-color="hsl(${b} 70% 38%)"/></linearGradient></defs><rect width="64" height="80" rx="10" fill="url(#t${escapeHtml(g.title_id)})"/><circle cx="52" cy="14" r="22" fill="#fff" fill-opacity=".08"/><path d="M0 62 L64 46 V80 H0z" fill="#000" fill-opacity=".18"/><text x="32" y="44" text-anchor="middle" font-family="Outfit, sans-serif" font-weight="700" font-size="22" fill="#fff">${escapeHtml(initials)}</text><text x="32" y="72" text-anchor="middle" font-family="Outfit, sans-serif" font-size="7" fill="#fff" fill-opacity=".8">${escapeHtml(g.title_id)}</text></svg>`;
}
const EMPTY_LIB_ART = `<svg viewBox="0 0 120 80" width="120" height="80" aria-hidden="true"><rect x="14" y="18" width="30" height="40" rx="6" fill="var(--elevated)" stroke="var(--border-strong)" transform="rotate(-8 29 38)"/><rect x="45" y="12" width="30" height="40" rx="6" fill="var(--elevated)" stroke="var(--accent)"/><rect x="76" y="18" width="30" height="40" rx="6" fill="var(--elevated)" stroke="var(--border-strong)" transform="rotate(8 91 38)"/><path d="M54 32h12M60 26v12" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round"/><ellipse cx="60" cy="72" rx="40" ry="3" fill="var(--border)"/></svg>`;
function filterLibraryGames(games) {
  const q = (libraryView?.q ?? N.settings.libQuery).trim().toLowerCase();
  const kind = libraryView?.filterKind ?? N.settings.libFilterKind;
  let list = games.filter((g) => {
    if (kind === "god" && g.kind !== "god" && g.kind !== "disc") return false;
    if (kind === "xbla" && g.kind !== "xbla") return false;
    if (kind === "xbox" && g.kind !== "xbox") return false;
    if (kind === "app" && g.kind !== "app") return false;
    if (kind === "xex" && g.kind !== "extracted") return false;
    if (kind === "dlc" && !g.dlc_count) return false;
    if (kind === "tu" && !g.tu_count) return false;
    if (!q) return true;
    return g.name.toLowerCase().includes(q) || g.title_id.toLowerCase().includes(q);
  });
  const sort = libraryView?.sort ?? N.settings.libSort;
  const cmp = {
    name: (a, b) => a.name.localeCompare(b.name),
    title: (a, b) => a.title_id.localeCompare(b.title_id),
    size: (a, b) => (b.game_size || 0) - (a.game_size || 0),
    dlc: (a, b) => (b.dlc_count || 0) - (a.dlc_count || 0),
    tu: (a, b) => (b.tu_count || 0) - (a.tu_count || 0),
  }[sort] || ((a, b) => a.name.localeCompare(b.name));
  list = [...list].sort(cmp);
  return list;
}
const LIB_SCAN_TIMEOUT_MS = 10 * 60 * 1000;
const LIB_TIMED_OUT = "Library scan timed out. Try a narrower folder (e.g. Content\\0000000000000000).";
const LIB_KIND_LABEL = { god: "GOD", disc: "Disc", xbla: "XBLA", xbox: "Xbox", app: "App", extracted: "XEX" };
function libraryKindLabel(k) { return LIB_KIND_LABEL[k] || k; }
let libCoverRun = 0;
async function loadLibraryCovers() {
  const run = ++libCoverRun;
  const view = libraryView;
  if (!view || !N.IS_DESKTOP) return;
  const games = view.allGames || [];
  const hydrate = async (g) => {
    if (run !== libCoverRun || view !== libraryView || view.covers[g.path]) return;
    try {
      // Local cover beside the game, else the disk cache; no network here.
      const src = await N.invoke("library_cover", { titleId: g.title_id, gamePath: g.path, allowCoverNetwork: false });
      if (src) view.covers[g.path] = src;
    } catch {}
  };
  for (let i = 0; i < games.length; i += 8) {
    if (run !== libCoverRun || view !== libraryView) return;
    await Promise.all(games.slice(i, i + 8).map(hydrate));
    if (view === libraryView) render();
  }
  if (!N.settings.coverArtNetwork) return;
  for (let i = 0; i < games.length; i += 4) {
    if (run !== libCoverRun || view !== libraryView || !N.settings.coverArtNetwork) return;
    await Promise.all(games.slice(i, i + 4).map(async (g) => {
      if (view.covers[g.path]) return;
      try {
        const path = await N.invoke("library_cover", { titleId: g.title_id, gamePath: g.path, allowCoverNetwork: true });
        if (path) view.covers[g.path] = path;
      } catch {}
    }));
    if (view === libraryView) render();
  }
}
// Every running library_index job I started (the visible scan and a background refresh).
const libScanIds = new Set();
// One way to stop library scans: close, Escape, Refresh/Ctrl+L starting a new one, the timeout.
function cancelLibraryScan(id) {
  for (const s of id ? [id] : [...libScanIds]) {
    libScanIds.delete(s);
    N.invoke("cancel_job", { id: s }).catch(() => {});
  }
}
function closeLibrary() {
  cancelLibraryScan();
  libCoverRun += 1;
  libraryView = null;
  render();
}
function libraryIndex(libs, maxAgeSecs, scanId) {
  libScanIds.add(scanId);
  return N.invoke("library_index", { libraries: libs, allowCoverNetwork: N.settings.coverArtNetwork, maxAgeSecs, scanId })
    .finally(() => libScanIds.delete(scanId));
}

async function openLibrary(forceFresh = false) {
  cancelLibraryScan(); // a new scan replaces any one still running
  if (forceFresh && N.IS_DESKTOP) N.invoke("library_cover_reset").catch(() => {}); // Refresh retries covers that failed before
  const scanId = uid();
  libraryView = {
    loading: true,
    scanId,
    scanMessage: "",
    games: [],
    allGames: [],
    stale: false,
    scannedAt: 0,
    copying: {},
    q: N.settings.libQuery,
    sort: N.settings.libSort,
    filterKind: N.settings.libFilterKind,
    covers: {},
  };
  render();
  const offProgress = N.onLibraryScanProgress(scanId, (p) => {
    if (!libraryView || libraryView.scanId !== scanId) return;
    libraryView.scanMessage = p.message || libraryView.scanMessage;
    render();
  });
  const libs = dlcLibraries();
  const maxAge = forceFresh ? 0 : 3600;
  let timedOut = false;
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => {
      timedOut = true;
      cancelLibraryScan(scanId); // stop the Rust side too, so it doesn't keep walking the drive
      reject(new Error(LIB_TIMED_OUT));
    }, LIB_SCAN_TIMEOUT_MS);
  });
  try {
    const idx = await Promise.race([libraryIndex(libs, maxAge, scanId), timeout]);
    libraryView.allGames = idx.games || [];
    libraryView.stale = !!idx.stale;
    libraryView.scannedAt = idx.scanned_at || 0;
    if (idx.scan_warnings?.length) toast(idx.scan_warnings.slice(0, 3).join("\n"));
    if (idx.cached && !forceFresh) {
      const view = libraryView;
      libraryIndex(libs, 0, uid())
        .then((fresh) => { if (libraryView === view && fresh?.games) { view.allGames = fresh.games; view.games = filterLibraryGames(view.allGames); view.scannedAt = fresh.scanned_at || 0; render(); loadLibraryCovers(); } })
        .catch(() => {});
    }
    loadLibraryCovers();
  } catch (e) {
    const cancelled = N.errCode(e) === "CANCELLED";
    if (libraryView?.scanId === scanId) toast(timedOut ? LIB_TIMED_OUT : cancelled ? "Library scan cancelled." : String(e));
    // After Cancel or the timeout, don't start an uncancellable full scan as a fallback.
    if (!timedOut && !cancelled && libraryView?.scanId === scanId) libraryView.allGames = await N.invoke("library_scan", { libraries: libs }).catch(() => []);
  } finally {
    clearTimeout(timer);
    offProgress();
    if (libraryView?.scanId === scanId) {
      libraryView.games = filterLibraryGames(libraryView.allGames);
      libraryView.loading = false;
      render();
    }
  }
}

function renderFolderConfirm() {
  if (!folderConfirm) return "";
  const f = folderConfirm;
  return `<div class="overlay" data-folder-overlay><div class="dialog" role="dialog" aria-modal="true"><h3>Add archives from folder?</h3>
    <p>${f.archives.length} archive${f.archives.length === 1 ? "" : "s"} / ISO found under <code>${escapeHtml(f.folder)}</code>. Add them to the extract queue? (Create archive is still a separate button.)</p>
    <ul class="sc-list" class="sc-list sc-list-scroll">${f.archives.slice(0, 40).map((p) => `<li><span>${escapeHtml(N.baseName(p))}</span></li>`).join("")}${f.archives.length > 40 ? `<li class="meta">…and ${f.archives.length - 40} more</li>` : ""}</ul>
    <div class="actions actions-end"><button class="btn btn-quiet" data-folder-cancel type="button">Cancel</button><button class="btn btn-primary" data-folder-ok type="button">Add ${f.archives.length} to queue</button></div>
  </div></div>`;
}

function renderLibrary() {
  if (!libraryView) return "";
  const L = libraryView;
  const libBar = `<div class="lib-bar">
        <input type="search" data-lib-q value="${escapeHtml(L.q)}" placeholder="Search name or TitleID" aria-label="Search library"/>
        <select data-lib-sort aria-label="Sort by"><option value="name" ${L.sort === "name" ? "selected" : ""}>Name</option><option value="title" ${L.sort === "title" ? "selected" : ""}>TitleID</option><option value="size" ${L.sort === "size" ? "selected" : ""}>Size</option><option value="dlc" ${L.sort === "dlc" ? "selected" : ""}>DLC count</option><option value="tu" ${L.sort === "tu" ? "selected" : ""}>TU count</option></select>
        <select data-lib-filter aria-label="Filter"><option value="all" ${L.filterKind === "all" ? "selected" : ""}>All</option><option value="god" ${L.filterKind === "god" ? "selected" : ""}>GOD / Disc</option><option value="xbla" ${L.filterKind === "xbla" ? "selected" : ""}>XBLA</option><option value="xbox" ${L.filterKind === "xbox" ? "selected" : ""}>Xbox originals</option><option value="app" ${L.filterKind === "app" ? "selected" : ""}>Apps</option><option value="xex" ${L.filterKind === "xex" ? "selected" : ""}>Extracted (XEX)</option><option value="dlc" ${L.filterKind === "dlc" ? "selected" : ""}>Has DLC</option><option value="tu" ${L.filterKind === "tu" ? "selected" : ""}>Has TU</option></select>
        ${L.stale ? `<span class="pill warn-text" title="Showing a fresh scan — cached index was older than an hour">Stale index</span>` : ""}
      </div>`;
  const body = L.loading ? `<div class="preview-empty"><span class="spin">◌</span> ${escapeHtml(L.scanMessage || "Looking in your library folders…")}${L.scanId ? ` <button class="btn btn-quiet btn-sm" data-lib-cancel type="button">Cancel</button>` : ""}</div>`
    : `${libBar}${!L.games.length ? `<div class="preview-empty lib-empty">${EMPTY_LIB_ART}<div><strong>No games found yet</strong><br>Add the folder with your GOD games (or a USB drive's Content folder) in Settings → Game library folders.</div></div>`
    : `<div class="lib-grid">${L.games.map((g, i) => { const c = L.copying[g.path]; return `<div class="lib-card">
        ${gameTile(g, L.covers?.[g.path])}
        <div class="lib-info"><strong title="${escapeHtml(g.path)}">${escapeHtml(g.name)}</strong>
          <span>${escapeHtml(g.title_id)} · ${g.size_approx ? "about " : ""}${formatBytes(g.game_size)} · <span class="pill">${escapeHtml(libraryKindLabel(g.kind))}</span></span>
          <span><span class="pill">${g.dlc_count} DLC</span> ${g.dlc_count ? formatBytes(g.dlc_size) : ""} ${g.tu_count ? `<span class="pill">${g.tu_count} TU</span>` : ""}</span>
          ${c ? `<div class="qbar"><div data-job-bar="${escapeHtml(c.id)}" class="qbar-fill"></div></div><span class="${c.error ? "danger" : ""}" data-job-status="${escapeHtml(c.id)}">${escapeHtml(c.error || c.status)}</span>` : ""}
          <div class="actions"><button class="btn btn-quiet btn-sm" data-lib-show="${i}" type="button" title="Show this game's folder in Explorer">Show</button><button class="btn btn-secondary btn-sm" data-lib-usb="${i}" type="button" title="Copy the game with its DLC and title updates to a USB drive's Content folder" ${c && !c.done && !c.error ? "disabled" : ""}>${icon("out")} Copy to USB…</button>${N.IS_DESKTOP ? `<button class="btn btn-secondary btn-sm" data-lib-xbox="${i}" type="button" title="Send this game to your Xbox over WiFi / LAN (FTP)">${SV(16, 16, ICONS.xbox)} Send to Xbox</button>` : ""}</div>
        </div></div>`; }).join("")}</div>`}`;
  return `<div class="overlay" data-lib-overlay><div class="dialog dialog-xl" data-library role="dialog" aria-modal="true" aria-labelledby="lib-h">
    <h3 id="lib-h">${icon("disc")} Library <span class="meta meta-h">${L.games.length ? L.games.length + " game" + (L.games.length === 1 ? "" : "s") : ""}</span></h3>
    ${body}
    <div class="actions actions-end-mt"><button class="btn btn-secondary" data-lib-refresh type="button" title="Scan the library folders again">Refresh</button><button class="btn btn-primary" data-lib-close type="button">Done</button></div>
  </div></div>`;
}
async function copyGameToUsb(g) {
  const target = await N.pickFolder("Pick the USB drive (its top folder). The game goes into Content\\0000000000000000.");
  if (!target) return;
  const c = { id: uid(), progress: 0, status: "Checking space…", error: "", done: false };
  libraryView.copying[g.path] = c;
  render();
  const off = N.onJobProgress(c.id, (p) => { c.progress = p.percent; c.status = `Copying ${p.percent}% · ${p.file}`; paintJob(c); });
  try {
    const r = await N.invoke("copy_to_usb", { id: c.id, game: g.path, target });
    c.progress = 100; c.done = true;
    c.status = `Copied ${r.copied} file${r.copied === 1 ? "" : "s"} (${formatBytes(r.bytes)})${r.identical ? `, ${r.identical} already there` : ""} → ${r.dest}`;
    addHistory({ name: g.name, source: g.path, output: r.dest, result: "created", label: "Copied to USB" });
    notifyDone("Unpack — copied to USB", g.name);
  } catch (err) {
    c.error = friendlyError(err);
    showError(`${g.name}: ${c.error}`);
  } finally { off(); render(); }
}
function bindFolderConfirm() {
  const root = app.querySelector("[data-folder-overlay]");
  if (!root) return;
  root.querySelector("[data-folder-cancel]")?.addEventListener("click", () => { folderConfirm = null; render(); });
  root.querySelector("[data-folder-ok]")?.addEventListener("click", async () => {
    const paths = folderConfirm.archives; folderConfirm = null; render();
    await addPaths(paths);
  });
  root.addEventListener("click", (e) => { if (e.target === e.currentTarget) { folderConfirm = null; render(); } });
}
function bindLibrary() {
  const root = app.querySelector("[data-library]");
  if (!root) return;
  const close = closeLibrary;
  root.querySelector("[data-lib-close]")?.addEventListener("click", close);
  root.querySelector("[data-lib-refresh]")?.addEventListener("click", () => openLibrary(true));
  root.querySelector("[data-lib-cancel]")?.addEventListener("click", () => { if (libraryView?.scanId) cancelLibraryScan(libraryView.scanId); });
  root.querySelector("[data-lib-q]")?.addEventListener("input", (e) => { libraryView.q = e.target.value; N.settings.libQuery = e.target.value; libraryView.games = filterLibraryGames(libraryView.allGames); const pos = e.target.selectionStart; render(); const n = app.querySelector("[data-lib-q]"); n?.focus(); n?.setSelectionRange(pos, pos); });
  root.querySelector("[data-lib-sort]")?.addEventListener("change", (e) => { libraryView.sort = e.target.value; N.settings.libSort = e.target.value; libraryView.games = filterLibraryGames(libraryView.allGames); render(); });
  root.querySelector("[data-lib-filter]")?.addEventListener("change", (e) => { libraryView.filterKind = e.target.value; N.settings.libFilterKind = e.target.value; libraryView.games = filterLibraryGames(libraryView.allGames); render(); });
  app.querySelector("[data-lib-overlay]")?.addEventListener("click", (e) => { if (e.target === e.currentTarget) close(); });
  root.querySelectorAll("[data-lib-show]").forEach((b) => b.addEventListener("click", () => { const g = libraryView.games[+b.dataset.libShow]; window.__TAURI__.opener?.revealItemInDir(g.path).catch((e) => toast(String(e))); }));
  root.querySelectorAll("[data-lib-usb]").forEach((b) => b.addEventListener("click", () => copyGameToUsb(libraryView.games[+b.dataset.libUsb])));
  root.querySelectorAll("[data-lib-xbox]").forEach((b) => b.addEventListener("click", () => sendGameToXbox(libraryView.games[+b.dataset.libXbox])));
}

// ---- Send to Xbox: FTP upload to a modded console on the LAN (Aurora / XeXMenu / DashLaunch) ----
// All networking is in Rust (ftp.rs); LAN IPs only, one connection at a time, never overwrites.
const XBOX_KINDS = { god: "GOD game → Content\\…\\<TitleID>\\00007000", dlc: "DLC → Content\\…\\<TitleID>\\00000002", tu: "Title update → Content\\…\\<TitleID>\\000B0000", xex: "Extracted XEX game → Games\\<name>", plugins: "Mod / plugin → Plugins", custom: "Other (pick a folder)" };
const XBOX_ART = `<svg viewBox="0 0 120 80" width="120" height="80" aria-hidden="true"><rect x="10" y="30" width="36" height="26" rx="5" fill="var(--elevated)" stroke="var(--border-strong)"/><path d="M18 60h20" stroke="var(--border-strong)" stroke-width="2.5" stroke-linecap="round"/><ellipse cx="88" cy="44" rx="22" ry="16" fill="var(--elevated)" stroke="var(--accent)"/><circle cx="88" cy="44" r="6" fill="none" stroke="var(--accent2)" stroke-width="2.5"/><path d="M52 34c6-6 14-6 20 0M56 40c4-3 8-3 12 0" fill="none" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" class="sum-ring"/><ellipse cx="60" cy="74" rx="44" ry="3" fill="var(--border)"/></svg>`;
let xboxView = null; // { con, consoles, path, entries, drives, status, busy, sends: [], kind, tid, prep }
const blankConsole = () => ({ name: "My Xbox", host: "", port: 21, user: "xboxftp", pass: "xboxftp", parallel: false });
// FTP errors come from Rust already worded for people; don't run them through the archive mapper.
const xboxError = (err) => String(err?.message || err || "Unknown error");
function xboxCon() { const c = xboxView.con; return { host: c.host.trim(), port: Number(c.port) || 21, user: c.user || "xboxftp", pass: c.pass ?? "" }; }
function openXbox() {
  const consoles = N.settings.consoles;
  if (!xboxView) xboxView = { con: { ...(consoles[0] || blankConsole()) }, path: "/", entries: null, drives: [], status: "", error: "", busy: false, sends: [], ftpLock: null };
  xboxView.consoles = consoles;
  xboxView.hidden = false;
  render();
  refreshXboxLock();
  startXboxLockPoll();
  if (xboxView.con.host && !xboxView.entries) xboxBrowse("/");
}
async function xboxBrowse(path) {
  const X = xboxView;
  if (!X.con.host.trim()) { X.error = "Enter the console's IP first (Aurora shows it on the main screen)."; render(); return; }
  X.busy = true; X.error = ""; X.status = `Connecting to ${X.con.host}…`; render();
  try {
    const entries = await N.invoke("ftp_list", { ...xboxCon(), path });
    if (!xboxView) return;
    X.path = path; X.entries = entries;
    if (path === "/") {
      X.drives = rootDrives(entries);
      // Items queued before the drive list was known follow the default for this console.
      const d = xboxDrive();
      X.sends.filter((o) => !o.running && !o.edited && !X.drives.includes(o.drive)).forEach((o) => { o.drive = d; xboxDefaultDest(o); });
    }
    X.status = `Connected · ${path}`;
  } catch (err) { X.error = xboxError(err); X.status = ""; showError(X.error); } finally { X.busy = false; render(); }
}
function xboxSaveConsole() {
  if (!/^\d{1,3}(\.\d{1,3}){3}$/.test(xboxView.con.host.trim())) { xboxView.error = "Enter the console's IP (like 192.168.1.50) before saving."; render(); return; }
  const X = xboxView, c = { ...X.con, host: X.con.host.trim(), port: Number(X.con.port) || 21, name: (X.con.name || "My Xbox").slice(0, 60) };
  const list = N.settings.consoles.filter((x) => x.host !== c.host);
  list.unshift(c);
  N.settings.consoles = list.slice(0, 10);
  X.consoles = N.settings.consoles;
  toast(`Saved ${c.name} (${c.host})`);
  render();
}
// Console drives: every drive the console lists at its root (Hdd1, HddX, Usb0, Usb1, UsbMu0…).
// Default: the drive remembered for this console if it's there, else Hdd1, else the first USB,
// else the first drive. The user's pick is remembered per console (host:port).
const DRIVE_RE = /^[A-Za-z][A-Za-z0-9]{0,15}$/;
function consoleKey(c) { const p = Number(c?.port); return `${String(c?.host || "").trim()}:${Number.isInteger(p) && p >= 1 && p <= 65535 ? p : 21}`; }
function rememberedDrive(c) { return N.settings.xboxDrives[consoleKey(c)] || ""; }
function rememberDrive(c, d) { if (c?.host && DRIVE_RE.test(d || "")) N.settings.xboxDrives = { ...N.settings.xboxDrives, [consoleKey(c)]: d }; }
function pickDrive(drives, remembered) {
  const hit = remembered && drives.find((d) => d.toLowerCase() === remembered.toLowerCase());
  return hit || drives.find((d) => /^hdd1$/i.test(d)) || drives.find((d) => /^usb/i.test(d)) || drives[0] || remembered || "Hdd1";
}
function rootDrives(entries) { return entries.filter((e) => e.is_dir && DRIVE_RE.test(e.name)).map((e) => e.name); }
function xboxDrive() { return pickDrive(xboxView.drives, rememberedDrive(xboxView.con)); }
function xboxSetDrive(d) {
  const X = xboxView;
  rememberDrive(X.con, d);
  X.sends.filter((o) => !o.running && !o.edited && o.kind !== "custom").forEach((o) => { o.drive = d; xboxDefaultDest(o); });
  render();
}
// prep: { src, name, kind, tid } from Library / Mods / a pick.
async function xboxPick(folder) {
  const paths = folder ? [await N.pickFolder("Pick the folder to send to the Xbox")].filter(Boolean) : await N.pickFiles("Pick files to send to the Xbox");
  for (const p of paths) {
    let kind = "custom", tid = "";
    try {
      const [r] = await N.invoke("mod_identify", { paths: [p] });
      const info = r?.Ok;
      if (info?.kind === "stfs" && info.title_id) { tid = info.title_id; kind = info.content_type === "00000002" ? "dlc" : info.content_type === "000B0000" ? "tu" : info.content_type === "00007000" ? "god" : "custom"; }
      else if (info?.kind === "folder" && info.title_id) { kind = "xex"; tid = info.title_id; }
      else if (info?.kind === "xex") kind = "plugins";
    } catch {}
    xboxQueue({ src: p, name: N.baseName(p), kind, tid });
  }
}
function xboxQueue(prep) {
  const X = xboxView;
  const s = { id: uid(), ...prep, drive: xboxDrive(), custom: X.path === "/" ? "" : X.path, dest: "", edited: false, destError: "", progress: 0, status: "Ready", error: "", done: false, running: false };
  X.sends.push(s);
  render();
  xboxDefaultDest(s);
}
function xboxArgs(s, withDest) { return { ...xboxCon(), src: s.src, kind: s.kind, drive: s.drive, titleId: s.tid || null, custom: s.kind === "custom" ? s.custom || null : null, parallel: !!xboxView.con.parallel, dest: withDest && s.edited ? s.dest : null }; }
// Pre-fill the standard folder for the item's type (unless the user edited it).
async function xboxDefaultDest(s) {
  if (s.edited) return xboxCheckDest(s);
  try { s.dest = (await N.invoke("ftp_dest", { args: xboxArgs(s, false) })).path; s.destError = ""; }
  catch (err) { s.dest = ""; s.destError = xboxError(err); }
  if (xboxView) render();
}
async function xboxCheckDest(s) {
  try { s.dest = (await N.invoke("ftp_dest", { args: xboxArgs(s, true) })).path; s.destError = ""; }
  catch (err) { s.destError = xboxError(err); }
  if (xboxView) render();
}
function xboxSetDest(s, path) { s.dest = path; s.edited = true; xboxCheckDest(s); }
async function xboxRun(s) {
  if (s.running) return;
  const X = xboxView;
  if (!(await preflightDest(s.dest || X.path, [s.src]))) return;
  s.running = true; s.error = ""; s.done = false; s.status = "Connecting…"; s.id = uid(); render();
  const off = N.onJobProgress(s.id, (p) => { s.progress = p.percent; s.status = p.percent >= 100 ? `Verifying… ${p.file}` : `Sending ${p.percent}% · ${p.file}`; paintJob(s); });
  try {
    const r = await N.invoke("ftp_send", { id: s.id, args: xboxArgs(s, true) });
    if (s.edited) N.settings.xboxLastDest = { ...N.settings.xboxLastDest, [s.kind]: r.dest };
    s.progress = 100; s.done = true;
    s.status = `Sent ${r.sent} file${r.sent === 1 ? "" : "s"} (${formatBytes(r.bytes)})${r.resumed ? `, ${r.resumed} resumed` : ""}${r.skipped ? `, ${r.skipped} already there` : ""}${r.links_skipped ? `, ${r.links_skipped} link${r.links_skipped === 1 ? "" : "s"} skipped` : ""} → ${r.dest} · ${r.sent ? `${(r.bytes / 1048576 / Math.max(r.seconds, 0.001)).toFixed(1)} MB/s avg${r.connections > 1 ? " over 2 connections" : ""} · ` : ""}sizes verified`;
    addHistory({ name: s.name, source: s.src, output: `${X.con.host}:${r.dest}`, result: "created", label: "Sent to Xbox" });
    notifyDone("Unpack — sent to Xbox", s.name);
  } catch (err) {
    s.error = String(err).includes("CANCELLED") ? "Cancelled. Press Resume to continue where it stopped." : xboxError(err);
    if (!String(err).includes("CANCELLED")) showError(`${s.name}: ${s.error}`);
  } finally { s.running = false; off(); render(); }
}
function renderXbox() {
  if (!xboxView || xboxView.hidden) return "";
  const X = xboxView, c = X.con;
  const drives = X.drives.length ? X.drives : [...new Set([xboxDrive(), ...X.sends.map((o) => o.drive)])];
  const crumbs = ["/", ...X.path.split("/").filter(Boolean)];
  const browser = X.entries === null ? `<div class="preview-empty lib-empty">${XBOX_ART}<div><strong>Connect to your console</strong><br>Turn on the FTP server in Aurora (or run XeXMenu / DashLaunch), enter the IP shown on the console and press Connect. Default login: xboxftp / xboxftp.</div></div>`
    : `<div class="xb-crumbs">${crumbs.map((s, k) => `<button class="chip" data-xb-crumb="${k}" type="button">${escapeHtml(k ? s : "Console")}</button>`).join("<span class=meta>›</span>")}</div>
      <ul class="xb-list">${X.entries.length ? X.entries.map((e, k) => `<li>${e.is_dir ? `<button class="xb-dir" data-xb-open="${k}" type="button">${icon("folder")} ${escapeHtml(e.name)}</button>` : `<span class="xb-file">${icon("file")} ${escapeHtml(e.name)}</span><span class="meta">${formatBytes(e.size)}</span>`}${X.path !== "/" ? `<button class="btn btn-quiet btn-sm" data-xb-ren="${k}" type="button" title="Rename">Ren</button><button class="btn btn-quiet btn-sm" data-xb-del="${k}" type="button" title="Delete">Del</button>` : ""}</li>`).join("") : `<li class="meta">Empty folder</li>`}</ul>`;
  const sends = X.sends.map((s, i) => { const last = N.settings.xboxLastDest[s.kind]; const lock = s.running ? "disabled" : ""; return `<div class="xb-send" data-xb-item="${i}">
      <div class="xb-send-head"><strong title="${escapeHtml(s.src)}">${escapeHtml(s.name)}</strong>
        <select data-xb-kind="${i}" ${lock} aria-label="Type">${Object.entries(XBOX_KINDS).map(([k, v]) => `<option value="${k}" ${s.kind === k ? "selected" : ""}>${escapeHtml(v)}</option>`).join("")}</select>
        <select data-xb-drive="${i}" ${s.running || s.kind === "custom" ? "disabled" : ""} aria-label="Drive">${drives.map((d) => `<option ${s.drive === d ? "selected" : ""}>${escapeHtml(d)}</option>`).join("")}</select>
        ${["god", "dlc", "tu"].includes(s.kind) ? `<input data-xb-tid="${i}" value="${escapeHtml(s.tid || "")}" maxlength="8" class="inp-w65" placeholder="TitleID" aria-label="TitleID" ${lock}/>` : ""}
      </div>
      <div class="xb-dest"><span class="meta">To</span><input data-xb-dest="${i}" value="${escapeHtml(s.dest || "")}" aria-label="Destination folder on the console" spellcheck="false" ${lock} class="${s.destError ? "bad" : ""}"/>
        <button class="btn btn-quiet btn-sm" data-xb-browse="${i}" type="button" ${lock} title="Pick the folder in the console browser on the right">Browse…</button>
        <button class="btn btn-quiet btn-sm" data-xb-reset="${i}" type="button" ${lock || (!s.edited ? "disabled" : "")} title="Back to the standard folder for this type">Reset to default</button>
        ${s.edited ? `<span class="pill pill-soft">custom</span>` : `<span class="pill">default</span>`}
        ${last && last !== s.dest && !s.running ? `<button class="chip" data-xb-last="${i}" type="button" title="Your last custom folder for this type">Last: ${escapeHtml(last)}</button>` : ""}
        ${N.settings.destPresets.length ? `<select data-xb-preset="${i}" aria-label="Destination preset"><option value="">Preset…</option>${N.settings.destPresets.map((pr, pi) => `<option value="${pi}">${escapeHtml(pr.name)}</option>`).join("")}</select>` : ""}</div>
      ${s.destError ? `<div class="danger" data-xb-desterr="${i}">${escapeHtml(s.destError)}</div>` : ""}
      <div class="qbar"><div data-job-bar="${escapeHtml(s.id)}" class="qbar-fill"></div></div>
      <div class="xb-send-foot"><span class="${s.error ? "danger" : "meta"}" data-job-status="${escapeHtml(s.id)}">${escapeHtml(s.error || s.status)}</span>
        <span class="actions">${s.running ? `<button class="btn btn-quiet btn-sm" data-xb-cancel="${i}" type="button">Cancel</button>` : `<button class="btn btn-primary btn-sm" data-xb-go="${i}" type="button" ${X.busy || s.destError || X.sends.some((o) => o.running) ? "disabled" : ""}>${s.error && /Resume|resume/.test(s.error) ? "Resume" : s.done ? "Send again" : `${SV(16, 16, ICONS.xbox)} Send`}</button><button class="btn btn-quiet btn-sm" data-xb-remove="${i}" type="button">Remove</button>`}</span></div>
    </div>`; }).join("");
  return `<div class="overlay" data-xb-overlay><div class="dialog dialog-xl" data-xbox role="dialog" aria-modal="true" aria-labelledby="xb-h">
    <h3 id="xb-h">${SV(18, 18, ICONS.xbox)} Send to Xbox <span class="meta meta-h">FTP over WiFi / LAN</span></h3>
    <div class="xb-con">
      ${X.consoles.length ? `<select data-xb-saved aria-label="Saved consoles"><option value="">Saved consoles…</option>${X.consoles.map((k, j) => `<option value="${j}">${escapeHtml(k.name)} · ${escapeHtml(k.host)}</option>`).join("")}</select>` : ""}
      <input data-xb-f="name" value="${escapeHtml(c.name)}" class="inp-w8" placeholder="Name" aria-label="Console name"/>
      <input data-xb-f="host" value="${escapeHtml(c.host)}" class="inp-w13" placeholder="Console IP, e.g. 192.168.1.50" aria-label="Console IP"/>
      <input data-xb-f="port" value="${escapeHtml(String(c.port))}" class="inp-w4" aria-label="Port" inputmode="numeric"/>
      <input data-xb-f="user" autocomplete="off" value="${escapeHtml(c.user)}" class="inp-w7" aria-label="FTP user"/>
      <input data-xb-f="pass" type="password" autocomplete="off" value="${escapeHtml(c.pass)}" class="inp-w7" aria-label="FTP password"/>
      <label class="meta" title="Use 2 transfers at once if the console's FTP server allows a second connection (falls back to 1 automatically)"><input type="checkbox" data-xb-parallel ${c.parallel ? "checked" : ""}/> 2 at once</label>
      <button class="btn btn-primary btn-sm" data-xb-connect type="button" ${X.busy ? "disabled" : ""} title="Log in and list the console's drives">${X.busy ? `<span class="spin">◌</span>` : ""} Connect / Test</button>
      <button class="btn btn-secondary btn-sm" data-xb-save type="button">Save console</button>
      <button class="btn btn-quiet btn-sm" data-xb-lan type="button" title="Scan your LAN for FTP (opt-in)">Discover LAN</button>
    </div>
    ${X.ftpLock?.busy ? `<div class="xb-lock" data-xb-lock>${escapeHtml(X.ftpLock.subsystem || "FTP")} busy: ${escapeHtml(X.ftpLock.detail || "another job is using the console")}</div>` : ""}
    ${X.error ? `<div class="error-box danger" data-xb-error>${escapeHtml(X.error)}</div>` : X.status ? `<div class="meta" data-xb-status>${escapeHtml(X.status)}</div>` : ""}
    <div class="xb-body">
      <section class="xb-queue"><div class="actions actions-mb6"><button class="btn btn-secondary btn-sm" data-xb-files type="button">${SV(16, 16, ICONS.plus)} Add files…</button><button class="btn btn-secondary btn-sm" data-xb-folder type="button">${icon("folder")} Add folder…</button><button class="btn btn-quiet btn-sm" data-xb-lib type="button">From Library…</button><button class="btn btn-quiet btn-sm" data-xb-mods type="button">From Mod Menus…</button></div>
        ${sends || `<div class="meta">Nothing queued. Add files, or use <b>Send to Xbox</b> on a game in the Library or a mod in Mod Menus. Unpack detects GOD games, DLC, title updates and XEX games and picks the right folder.</div>`}
        <p class="meta meta-mt6">Uploads go to a <code>.part</code> file, are renamed when complete and their size is checked on the console. Existing files are never overwritten. Only local network addresses are allowed.</p></section>
      <section class="xb-browse">${X.drives.length ? `<div class="xb-drives" data-xb-drives><span class="meta">Send to drive</span><select data-xb-defdrive aria-label="Default drive for this console">${X.drives.map((d) => `<option ${xboxDrive() === d ? "selected" : ""}>${escapeHtml(d)}</option>`).join("")}</select><span class="meta">${X.drives.length} drive${X.drives.length === 1 ? "" : "s"} on this console${X.drives.filter((d) => /^usb/i.test(d)).length > 1 ? " · pick which USB" : ""}</span>
        <button class="btn btn-quiet btn-sm" data-xb-preset-save type="button" title="Save current folder as a named preset">Save preset</button></div>` : ""}${X.pickFor != null && X.sends[X.pickFor] ? `<div class="xb-pick" data-xb-pickbar>Pick a folder for <b>${escapeHtml(X.sends[X.pickFor].name)}</b>, then <button class="btn btn-primary btn-sm" data-xb-usehere type="button" ${X.entries && X.path !== "/" ? "" : "disabled"}>Use ${escapeHtml(X.path)}</button><button class="btn btn-quiet btn-sm" data-xb-pickcancel type="button">Cancel</button></div>` : ""}${browser}${X.entries ? `<div class="actions actions-mt5"><button class="btn btn-quiet btn-sm" data-xb-refresh type="button">Refresh</button>${X.path !== "/" ? `<button class="btn btn-quiet btn-sm" data-xb-mkdir type="button">New folder</button>` : ""}${X.sends.length && X.path !== "/" ? `<button class="btn btn-secondary btn-sm" data-xb-allhere type="button" title="Set every queued item's destination to this folder" ${X.sends.some((o) => o.running) ? "disabled" : ""}>Send all to this folder</button>` : ""}
        <button class="btn btn-quiet btn-sm" data-xb-save-back type="button" title="Download save from console to your PC">Backup save</button>
        <button class="btn btn-quiet btn-sm" data-xb-save-restore type="button" title="Upload save to console (backs up existing files first)">Restore save</button>
        <button class="btn btn-quiet btn-sm" data-xb-shots type="button" title="Aurora screenshot gallery">Screenshots</button></div>` : ""}</section>
    </div>
    <div class="actions actions-end-mt"><button class="btn btn-primary" data-xb-close type="button">Done</button></div>
  </div></div>${X.shots || X.shotsBusy ? `<div class="overlay" data-xb-shots-overlay><div class="dialog dialog-lg" data-xb-shots role="dialog" aria-modal="true">
    <h3>${icon("image")} Screenshot gallery <span class="meta">${escapeHtml(X.shots?.drive || xboxDrive() || "")}</span></h3>
    ${X.shotsBusy ? `<div class="meta"><span class="spin">◌</span> Listing screenshots…</div>` : `<div class="xb-shot-grid" data-xb-shot-grid>${(X.shots?.list || []).map((s, i) => `<figure class="xb-shot" data-xb-shot="${i}"><div class="xb-shot-thumb">${X.shots.thumbs?.[s.name] ? `<img src="${X.shots.thumbs[s.name]}" alt="">` : icon("image")}</div><figcaption><span class="meta">${escapeHtml(s.name)}</span><br>${formatBytes(s.size)}</figcaption></figure>`).join("") || `<div class="meta">No screenshots on this drive.</div>`}</div>
    <div class="actions"><button class="btn btn-primary btn-sm" data-xb-shots-dl type="button" ${X.shots?.list?.length ? "" : "disabled"}>Save all to PC…</button><button class="btn btn-quiet btn-sm" data-xb-shots-close type="button">Close</button></div>`}
  </div></div>` : ""}`;
}
async function xboxSavePickMeta() {
  const X = xboxView;
  const drive = xboxDrive() || X.drives[0];
  if (!drive || X.entries === null) { toast("Connect to the console first."); return null; }
  const profile = prompt("Profile folder under Content (e.g. 0000000000000000)", "0000000000000000");
  if (profile === null) return null;
  const prof = profile.trim();
  if (!prof || /[\\/]/.test(prof)) { toast("Profile must be a single folder name."); return null; }
  const tidRaw = prompt("Title ID (8 hex characters)", "4D5307E6");
  if (tidRaw === null) return null;
  const tid = tidRaw.trim().toUpperCase();
  if (!/^[\dA-F]{8}$/.test(tid)) { toast("Title ID must be 8 hex characters."); return null; }
  return { drive, profile: prof, title_id: tid };
}
async function xboxBackupSave() {
  const X = xboxView, meta = await xboxSavePickMeta();
  if (!meta) return;
  const dest = await N.pickFolder("Save backup into this folder on your PC");
  if (!dest) return;
  X.status = "Backing up save from console…"; render();
  try {
    const r = await N.invoke("saves_backup", { ...xboxCon(), ...meta, dest });
    toast(`Backed up ${r.files} file${r.files === 1 ? "" : "s"} (${formatBytes(r.bytes)}) → ${r.dest}`);
    X.status = "";
  } catch (err) { showError(xboxError(err)); X.status = ""; }
  render();
}
async function xboxRestoreSave() {
  const X = xboxView, meta = await xboxSavePickMeta();
  if (!meta) return;
  const src = await N.pickFolder("Pick the folder that contains the save files to restore");
  if (!src) return;
  if (!confirm("Restore uploads these files to the console. Any existing save files there are moved into a .unpack-save-backup folder on the console first, then sizes are verified. Continue?")) return;
  X.status = "Restoring save to console…"; render();
  try {
    const r = await N.invoke("saves_restore", { ...xboxCon(), ...meta, src });
    for (const msg of r.recovered || []) showMigrationNotice(msg);
    const extra = r.backed_up ? ` ${r.backed_up} existing file${r.backed_up === 1 ? "" : "s"} backed up on console (${r.backup_remote}).` : "";
    toast(`Restored ${r.uploaded} file${r.uploaded === 1 ? "" : "s"} (${formatBytes(r.bytes)}) → ${r.remote}.${extra}`);
    X.status = "";
  } catch (err) { showError(xboxError(err)); X.status = ""; }
  render();
}
async function xboxOpenScreenshots() {
  const X = xboxView;
  const drive = xboxDrive() || X.drives[0];
  if (!drive || X.entries === null) { toast("Connect to the console first."); return; }
  X.shotsBusy = true; X.shots = { drive, list: [], thumbs: {} }; render();
  try {
    const list = await N.invoke("screenshots_list", { ...xboxCon(), drive });
    X.shots = { drive, list, thumbs: {} };
    if (!list.length) toast("No screenshots found in Aurora folders on this drive.");
  } catch (err) { X.shots = null; showError(xboxError(err)); }
  finally { X.shotsBusy = false; render(); }
}
async function xboxPullScreenshots() {
  const X = xboxView;
  if (!X.shots?.list?.length) return;
  const dest = await N.pickFolder("Save screenshots into this folder");
  if (!dest) return;
  X.shotsBusy = true; render();
  try {
    const r = await N.invoke("screenshots_pull", { ...xboxCon(), shots: X.shots.list, dest });
    toast(`Saved ${r.files} screenshot${r.files === 1 ? "" : "s"} (${formatBytes(r.bytes)}) → ${r.dest}`);
    // Thumbnails come from the exact bytes Rust saved (a taken name is saved as name-2.png),
    // so a renamed copy never shows the old file (R21 L3).
    X.shots.thumbs = {};
    X.shots.list.forEach((s, i) => { const u = r.thumbs?.[i]; if (typeof u === "string" && /^data:image\/(png|jpeg|bmp);base64,[A-Za-z0-9+/=]+$/.test(u)) X.shots.thumbs[s.name] = u; });
  } catch (err) { showError(xboxError(err)); }
  finally { X.shotsBusy = false; render(); }
}
function bindXbox() {
  const root = app.querySelector("[data-xbox]");
  if (!root) return;
  const X = xboxView;
  const close = () => { if (X.sends.some((s) => s.running)) { toast("A send is still running. Cancel it first."); return; } xboxView = null; render(); };
  root.querySelector("[data-xb-close]")?.addEventListener("click", close);
  app.querySelector("[data-xb-overlay]")?.addEventListener("click", (e) => { if (e.target === e.currentTarget) close(); });
  root.querySelectorAll("[data-xb-f]").forEach((el) => el.addEventListener("input", () => { X.con[el.dataset.xbF] = el.value; }));
  root.querySelector("[data-xb-saved]")?.addEventListener("change", (e) => { const k = X.consoles[+e.target.value]; if (k) { X.con = { ...k }; X.entries = null; X.drives = []; xboxBrowse("/"); } });
  root.querySelector("[data-xb-parallel]")?.addEventListener("change", (e) => { X.con.parallel = e.target.checked; });
  root.querySelector("[data-xb-connect]")?.addEventListener("click", () => xboxBrowse("/"));
  root.querySelector("[data-xb-refresh]")?.addEventListener("click", () => xboxBrowse(X.path));
  root.querySelector("[data-xb-save]")?.addEventListener("click", xboxSaveConsole);
  root.querySelectorAll("[data-xb-crumb]").forEach((b) => b.addEventListener("click", () => xboxBrowse("/" + X.path.split("/").filter(Boolean).slice(0, +b.dataset.xbCrumb).join("/"))));
  root.querySelectorAll("[data-xb-open]").forEach((b) => b.addEventListener("click", () => { const e = X.entries[+b.dataset.xbOpen]; xboxBrowse((X.path === "/" ? "" : X.path) + "/" + e.name); }));
  root.querySelector("[data-xb-files]")?.addEventListener("click", () => xboxPick(false).catch((e) => toast(String(e))));
  root.querySelector("[data-xb-folder]")?.addEventListener("click", () => xboxPick(true).catch((e) => toast(String(e))));
  root.querySelector("[data-xb-lib]")?.addEventListener("click", () => { X.hidden = true; openLibrary(); });
  root.querySelector("[data-xb-mods]")?.addEventListener("click", () => { X.hidden = true; openMods(); });
  root.querySelectorAll("[data-xb-kind]").forEach((el) => el.addEventListener("change", () => { const s = X.sends[+el.dataset.xbKind]; s.kind = el.value; if (s.kind === "custom" && !s.custom && X.path !== "/") s.custom = X.path; xboxDefaultDest(s); }));
  root.querySelectorAll("[data-xb-drive]").forEach((el) => el.addEventListener("change", () => { const s = X.sends[+el.dataset.xbDrive]; s.drive = el.value; rememberDrive(X.con, el.value); xboxDefaultDest(s); }));
  root.querySelector("[data-xb-defdrive]")?.addEventListener("change", (e) => xboxSetDrive(e.target.value));
  root.querySelectorAll("[data-xb-tid]").forEach((el) => el.addEventListener("change", () => { const s = X.sends[+el.dataset.xbTid]; s.tid = el.value.trim().toUpperCase(); xboxDefaultDest(s); }));
  root.querySelectorAll("[data-xb-dest]").forEach((el) => { el.addEventListener("input", () => { const s = X.sends[+el.dataset.xbDest]; s.dest = el.value; s.edited = true; }); el.addEventListener("change", () => xboxCheckDest(X.sends[+el.dataset.xbDest])); });
  root.querySelectorAll("[data-xb-browse]").forEach((b) => b.addEventListener("click", () => { const s = X.sends[+b.dataset.xbBrowse]; X.pickFor = +b.dataset.xbBrowse; const segs = (s.dest || "").split("/").filter(Boolean); if (X.con.host) xboxBrowse(segs.length ? "/" + segs[0] : "/"); else render(); }));
  root.querySelectorAll("[data-xb-reset]").forEach((b) => b.addEventListener("click", () => { const s = X.sends[+b.dataset.xbReset]; s.edited = false; xboxDefaultDest(s); }));
  root.querySelectorAll("[data-xb-last]").forEach((b) => b.addEventListener("click", () => { const s = X.sends[+b.dataset.xbLast]; xboxSetDest(s, N.settings.xboxLastDest[s.kind]); }));
  root.querySelector("[data-xb-usehere]")?.addEventListener("click", () => { const s = X.sends[X.pickFor]; X.pickFor = null; if (s) xboxSetDest(s, X.path); });
  root.querySelector("[data-xb-pickcancel]")?.addEventListener("click", () => { X.pickFor = null; render(); });
  root.querySelector("[data-xb-allhere]")?.addEventListener("click", () => { const d = X.path.split("/").filter(Boolean)[0]; X.sends.filter((o) => !o.running).forEach((o) => { if (d && X.drives.includes(d)) o.drive = d; xboxSetDest(o, X.path); }); toast(`All ${X.sends.length} items will go to ${X.path}`); });
  root.querySelectorAll("[data-xb-go]").forEach((b) => b.addEventListener("click", () => xboxRun(X.sends[+b.dataset.xbGo])));
  root.querySelectorAll("[data-xb-cancel]").forEach((b) => b.addEventListener("click", () => N.invoke("cancel_job", { id: X.sends[+b.dataset.xbCancel].id }).catch(() => {})));
  root.querySelectorAll("[data-xb-remove]").forEach((b) => b.addEventListener("click", () => { X.sends.splice(+b.dataset.xbRemove, 1); render(); }));
  root.querySelectorAll("[data-xb-preset]").forEach((el) => el.addEventListener("change", () => {
    const i = +el.dataset.xbPreset;
    const s = X.sends[i];
    const p = N.settings.destPresets[+el.value];
    if (s && p) xboxSetDest(s, p.path);
    el.value = "";
  }));
  root.querySelector("[data-xb-preset-save]")?.addEventListener("click", () => {
    if (X.path === "/" || !X.path) { toast("Browse to a folder first."); return; }
    const name = prompt("Preset name", X.path.split("/").filter(Boolean).pop() || "Preset");
    if (!name) return;
    const list = [...N.settings.destPresets, { name: name.slice(0, 40), path: X.path }];
    N.settings.destPresets = list.slice(-20);
    toast("Saved preset " + name);
    render();
  });
  root.querySelector("[data-xb-lan]")?.addEventListener("click", async () => {
    X.status = "Scanning LAN…"; render();
    try {
      const found = await N.invoke("lan_discover", { host: X.con.host || null, port: X.con.port || 21 });
      if (!found.length) { toast("No FTP servers found on this LAN."); X.status = ""; render(); return; }
      const hit = found[0];
      X.con.host = hit.host;
      if (hit.port) X.con.port = hit.port;
      toast(`Found ${hit.host}:${hit.port || 21}`);
      xboxBrowse("/");
    } catch (err) { toast(String(err?.message || err)); X.status = ""; render(); }
  });
  root.querySelector("[data-xb-mkdir]")?.addEventListener("click", async () => {
    const name = prompt("New folder name");
    if (!name || /[\\/]/.test(name)) return;
    const path = (X.path === "/" ? "" : X.path) + "/" + name.trim();
    try {
      await N.invoke("ftp_mkdir", { ...xboxCon(), path });
      xboxBrowse(X.path);
    } catch (err) { showError(String(err?.message || err)); }
  });
  root.querySelectorAll("[data-xb-ren]").forEach((b) => {
    b.addEventListener("click", async () => {
      const e = X.entries[+b.dataset.xbRen];
      if (!e) return;
      if (/^launch\.ini$/i.test(e.name)) { toast("launch.ini can't be renamed here."); return; }
      const segs = X.path.split("/").filter(Boolean);
      if (segs.length < 2) { toast("Pick a folder inside a drive, not the drive root."); return; }
      const newName = prompt("Rename to", e.name);
      if (!newName || newName.includes("/")) return;
      const from = (X.path === "/" ? "" : X.path) + "/" + e.name;
      const to = (X.path === "/" ? "" : X.path) + "/" + newName.trim();
      if (!confirm(`Rename ${e.name} to ${newName}?`)) return;
      try {
        await N.invoke("ftp_rename", { ...xboxCon(), from, to });
        xboxBrowse(X.path);
      } catch (err) { showError(String(err?.message || err)); }
    });
  });
  root.querySelectorAll("[data-xb-del]").forEach((b) => {
    b.addEventListener("click", async () => {
      const e = X.entries[+b.dataset.xbDel];
      if (!e) return;
      if (/^launch\.ini$/i.test(e.name)) { toast("launch.ini can't be deleted."); return; }
      const segs = X.path.split("/").filter(Boolean);
      if (segs.length < 2) { toast("Can't delete drive roots."); return; }
      const path = (X.path === "/" ? "" : X.path) + "/" + e.name;
      if (!confirm(`Delete ${e.name} on the console? This can't be undone.`)) return;
      try {
        await N.invoke("ftp_delete", { ...xboxCon(), path });
        xboxBrowse(X.path);
      } catch (err) { showError(String(err?.message || err)); }
    });
  });
  root.querySelector("[data-xb-save-back]")?.addEventListener("click", () => xboxBackupSave().catch((e) => showError(String(e))));
  root.querySelector("[data-xb-save-restore]")?.addEventListener("click", () => xboxRestoreSave().catch((e) => showError(String(e))));
  root.querySelector("[data-xb-shots]")?.addEventListener("click", () => xboxOpenScreenshots().catch((e) => showError(String(e))));
  app.querySelector("[data-xb-shots-overlay]")?.addEventListener("click", (e) => { if (e.target === e.currentTarget) { xboxView.shots = null; render(); } });
  app.querySelector("[data-xb-shots-close]")?.addEventListener("click", () => { xboxView.shots = null; render(); });
  app.querySelector("[data-xb-shots-dl]")?.addEventListener("click", () => xboxPullScreenshots().catch((e) => showError(String(e))));
}
let xboxLockTimer = null;
async function refreshXboxLock() {
  if (!xboxView || xboxView.hidden) return;
  try {
    xboxView.ftpLock = await N.invoke("ftp_lock_status");
  } catch {
    xboxView.ftpLock = null;
  }
  const el = app.querySelector("[data-xb-lock]");
  if (el && xboxView.ftpLock?.busy) el.textContent = `${xboxView.ftpLock.subsystem || "FTP"} busy: ${xboxView.ftpLock.detail || ""}`;
}
function startXboxLockPoll() {
  clearInterval(xboxLockTimer);
  xboxLockTimer = setInterval(() => refreshXboxLock(), 2500);
}
// From Library / Mod Menus: close that window and queue the item in Send to Xbox.
function sendGameToXbox(g) { cancelLibraryScan(); libraryView = null; openXbox(); xboxQueue({ src: g.path, name: g.name, kind: g.kind === "extracted" ? "xex" : "god", tid: g.title_id }); }
function sendModToXbox(it) {
  const f = it.info, g = modGame(it);
  const kind = f?.kind === "stfs" && f.content_type === "00000002" ? "dlc" : f?.kind === "stfs" && f.content_type === "000B0000" ? "tu" : f?.kind === "xex" || it.mode === "plugins" ? "plugins" : "custom";
  modsView = null; openXbox(); xboxQueue({ src: it.path, name: f?.name || N.baseName(it.path), kind, tid: f?.title_id || g?.tid || "" });
}


// ---- Homebrew Tools: reference cards only (no downloads bundled). Opens real source pages. ----
const HOMEBREW_TOOLS = [
  { name: "Aurora Dashboard", purpose: "Feature-rich custom dashboard for RGH/JTAG consoles (game scanning, covers, scripts).", category: "Dashboards & File Explorers", url: "https://consolemods.org/wiki/Xbox_360:Homebrew_Apps_List" },
  { name: "XeXMenu 1.2", purpose: "Classic file manager / launcher used as a backup dashboard on modded consoles.", category: "Dashboards & File Explorers", url: "https://consolemods.org/wiki/Xbox_360:Homebrew_Apps_List" },
  { name: "DashLaunch", purpose: "Configures launch.ini (default dashboard, plugins, fans) on modded consoles.", category: "Dashboards & File Explorers", url: "https://consolemods.org/wiki/Xbox_360:DashLaunch" },
  { name: "XE Unshackle", purpose: "Temporary HV/kernel patch loader for BadUpdate; loads DashLaunch from USB.", category: "Dashboards & File Explorers", url: "https://github.com/Byrom90/XeUnshackle/releases" },
  { name: "Horizon", purpose: "PC content manager for Xbox 360 profiles, saves, and device browsing.", category: "PC Utilities", url: "https://github.com/unknownv2/horizon-v3" },
  { name: "Modio", purpose: "PC all-in-one for browsing Xbox 360 USB/HDD content and editing packages.", category: "PC Utilities", url: "https://consolemods.org/wiki/Xbox_360:USB_Mods" },
  { name: "Le Fluffie", purpose: "Open-source PC tool for profiles, achievements, and package security (rehash/resign).", category: "PC Utilities", url: "https://www.se7ensins.com/forums/threads/how-to-mod-achievements-using-le-fluffie.144443/" },
  { name: "Xbox 360 Hash Block Calculator", purpose: "Recalculates STFS hash blocks after editing a package or save.", category: "PC Utilities", url: "https://www.se7ensins.com/forums/threads/the-ultimate-mod-list.1193877/" },
  { name: "USBXTAFGUI", purpose: "Browse, extract, and inject files on Xbox 360–formatted USB drives.", category: "PC Utilities", url: "https://consolemods.org/wiki/Xbox_360:USB_Mods" },
  { name: "RDR Gamesave Editor", purpose: "Edit Red Dead Redemption Xbox 360 save games offline.", category: "Save Editors", url: "https://www.se7ensins.com/forums/threads/the-ultimate-mod-list.1193877/" },
  { name: "Saints Row save editor", purpose: "Edit Saints Row Xbox 360 save games offline.", category: "Save Editors", url: "https://www.se7ensins.com/forums/threads/the-ultimate-mod-list.1193877/" },
  { name: "WillowTree (Borderlands)", purpose: "Borderlands save editor for Xbox 360 (characters, items, stats).", category: "Save Editors", url: "https://consolemods.org/wiki/Xbox_360:USB_Mods" },
];
const HB_CATS = ["Dashboards & File Explorers", "PC Utilities", "Save Editors"];
let homebrewView = null;
function unwrapSourceUrl(raw) {
  try {
    const u = new URL(raw);
    if (/google\./i.test(u.hostname) && /\/url/.test(u.pathname)) {
      return u.searchParams.get("q") || u.searchParams.get("url") || raw;
    }
  } catch {}
  return raw;
}
async function openSourcePage(url) {
  const target = unwrapSourceUrl(url);
  try {
    if (window.__TAURI__?.opener?.openUrl) await window.__TAURI__.opener.openUrl(target);
    else if (window.__TAURI__?.core?.invoke) await N.invoke("plugin:opener|open_url", { url: target });
    else window.open(target, "_blank", "noopener");
  } catch (e) { toast(String(e)); }
}
function openHomebrew() { homebrewView = { q: "", cat: "" }; render(); }
function renderHomebrew() {
  if (!homebrewView) return "";
  const q = homebrewView.q.trim().toLowerCase();
  const items = HOMEBREW_TOOLS.filter((t) => (!homebrewView.cat || t.category === homebrewView.cat) && (!q || [t.name, t.purpose, t.category].some((x) => x.toLowerCase().includes(q))));
  const cards = items.map((t, i) => `<article class="hb-card" data-hb="${i}">
    <strong>${escapeHtml(t.name)}</strong>
    <span class="pill">${escapeHtml(t.category)}</span>
    <p class="meta">${escapeHtml(t.purpose)}</p>
    <div class="actions"><button class="btn btn-secondary btn-sm" data-hb-open="${HOMEBREW_TOOLS.indexOf(t)}" type="button">Open source page</button></div>
  </article>`).join("") || `<div class="preview-empty">Nothing matches.</div>`;
  return `<div class="overlay" data-hb-overlay><div class="dialog dialog-xl mods-dialog" data-homebrew role="dialog" aria-modal="true" aria-labelledby="hb-h">
    <h3 id="hb-h">${SV(18,18,ICONS.mod)} Homebrew Tools</h3>
    <p class="mods-note" role="note">Reference cards for PC/console utilities only — not online mod menus. Unpack never bundles these apps; each button opens the real source page in your browser. After you download a tool zip via <b>Download from link</b>, you can place it under <code>USB\\Apps\\&lt;Name&gt;</code>.</p>
    <div class="mods-bar">
      <input data-hb-q type="search" value="${escapeHtml(homebrewView.q)}" placeholder="Search tools…" aria-label="Search homebrew tools" />
      <select data-hb-cat aria-label="Category"><option value="">All categories</option>${HB_CATS.map((c) => `<option value="${escapeHtml(c)}" ${homebrewView.cat === c ? "selected" : ""}>${escapeHtml(c)}</option>`).join("")}</select>
    </div>
    <div class="hb-grid">${cards}</div>
    <div class="actions actions-end-mt"><button class="btn btn-primary" data-hb-close type="button">Done</button></div>
  </div></div>`;
}
function bindHomebrew() {
  const root = app.querySelector("[data-homebrew]");
  if (!root || !homebrewView) return;
  const close = () => { homebrewView = null; render(); };
  root.querySelector("[data-hb-close]")?.addEventListener("click", close);
  app.querySelector("[data-hb-overlay]")?.addEventListener("click", (e) => { if (e.target === e.currentTarget) close(); });
  const q = root.querySelector("[data-hb-q]");
  q?.addEventListener("input", () => { homebrewView.q = q.value; const pos = q.selectionStart; render(); const n = app.querySelector("[data-hb-q]"); n?.focus(); n?.setSelectionRange(pos, pos); });
  root.querySelector("[data-hb-cat]")?.addEventListener("change", (e) => { homebrewView.cat = e.target.value; render(); });
  root.querySelectorAll("[data-hb-open]").forEach((b) => b.addEventListener("click", () => openSourcePage(HOMEBREW_TOOLS[+b.dataset.hbOpen].url)));
}

function matchHomebrewName(filename) {
  const stem = String(filename || "").replace(/\.(zip|7z|rar|exe|msi)$/i, "").toLowerCase();
  return HOMEBREW_TOOLS.find((t) => stem.includes(t.name.toLowerCase().split(" ")[0].toLowerCase()) || t.name.toLowerCase().replace(/[^a-z0-9]/g, "").includes(stem.replace(/[^a-z0-9]/g, "").slice(0, 8)));
}
async function copyDownloadToUsbApps(filePath, displayName) {
  const appsDir = await N.pickFolder("Pick or create Apps\\" + displayName + " on the USB drive");
  if (!appsDir) return;
  const id = uid();
  const off = N.onJobProgress(id, () => {});
  try {
    const r = await N.invoke("mod_copy", { id, path: filePath, target: appsDir, mode: "as_is" });
    toast("Copied to " + r.dest);
    addHistory({ name: displayName, source: filePath, output: r.dest, result: "created", label: "Homebrew → USB Apps" });
  } catch (err) { toast(friendlyError(err)); }
  finally { off(); }
}

// ---- Mod Menus: a browser for mod files the user already has. Offline only; no downloads, no links. ----
let modsView = null; // { loading, items, q, fGame, fType, sel, pick: {q, results} }
const MODE_LABEL = { plugins: "Plugins folder (\\Plugins)", content: "Game's Content folder", as_is: "Top of the drive / folder, as is" };
const TYPE_LABEL = { xex: "XEX", stfs: "Package", folder: "Folder", file: "Other" };
function defaultMode(info) { return info?.kind === "stfs" ? "content" : info?.kind === "xex" ? "plugins" : "as_is"; }
/** The game for an item: header > the user's choice > a confident guess. */
function modGame(it) {
  const f = it.info;
  if (f?.title_id) return { tid: f.title_id, name: f.game || "Unknown game", how: "header", conf: 100 };
  if (it.tid) return { tid: it.tid, name: it.gname || it.tid, how: it.builtin ? "builtin" : "you", conf: 100 };
  const g = f?.guesses?.[0];
  if (g && f.confident) return { tid: g.title_id, name: g.name, how: "guess", conf: g.confidence, why: g.reason };
  return null;
}
const hintFor = (p) => N.baseName(N.dirName(N.dirName(p)));
async function openMods(tab = "library") {
  modsView = { loading: true, items: N.settings.mods.map((m) => ({ ...m })), q: "", fGame: "", fType: "", sel: 0, pick: null, tab, dl: { url: "", notes: "", name: "", game: "", sha: "", job: null, last: null }, links: N.settings.savedLinks.map((x) => ({ ...x })), names: N.settings.modNames, namePick: null };
  render();
  // Built-in menus (resources/menus/menus.json, ships empty): read-only items after the user's.
  const B = await N.invoke("builtin_menus").catch(() => ({ menus: [] }));
  if (!modsView) return;
  const have = new Set(modsView.items.map((i) => i.path));
  modsView.items.push(...(B.menus || []).filter((b) => !have.has(b.path)).map((b) => ({ path: b.path, notes: "", mode: "", tid: b.title_id, gname: b.game, builtin: { name: b.name, version: b.version, sha256: b.sha256 || "" } })));
  await identifyMods(modsView.items);
  modsView.loading = false;
  render();
}
async function identifyMods(items) {
  const res = await N.invoke("mod_identify", { paths: items.map((i) => i.path), hints: items.map((i) => hintFor(i.path)), learned: N.settings.modNames.map(({ key, tid }) => ({ key, tid })) }).catch((e) => items.map(() => ({ Err: String(e) })));
  items.forEach((it, k) => { const r = res[k] || {}; it.info = r.Ok || null; it.error = r.Err || ""; if (!it.mode) it.mode = defaultMode(it.info); });
}
function saveMods() { N.settings.mods = modsView.items.filter((i) => !i.builtin).map(({ path, notes, mode, tid, gname }) => ({ path, notes, mode, tid: tid || "", gname: gname || "" })); }
async function addMods(paths) {
  if (!modsView) await openMods();
  const have = new Set(modsView.items.map((i) => i.path));
  const fresh = paths.filter((p) => !have.has(p)).map((path) => ({ path, notes: "", mode: "", tid: "", gname: "" }));
  if (!fresh.length) return;
  await identifyMods(fresh);
  modsView.items.push(...fresh);
  modsView.sel = modsView.items.length - fresh.length;
  saveMods();
  render();
}
function modsVisible() {
  const M = modsView, q = M.q.trim().toLowerCase();
  return M.items.map((it, i) => [it, i]).filter(([it]) => {
    const g = modGame(it);
    if (M.fType && it.info?.kind !== M.fType) return false;
    if (M.fGame === "?" ? g : M.fGame && g?.tid !== M.fGame) return false;
    return !q || [it.info?.name, it.path, it.notes, g?.name, g?.tid].some((x) => String(x || "").toLowerCase().includes(q));
  });
}
function modTile(it) {
  const g = modGame(it);
  if (g) return gameTile({ title_id: g.tid, name: g.name });
  return `<svg class="tile" viewBox="0 0 64 80" aria-hidden="true"><rect width="64" height="80" rx="10" fill="var(--elevated)" stroke="var(--border-strong)" stroke-dasharray="4 3"/><text x="32" y="50" text-anchor="middle" font-family="Outfit, sans-serif" font-weight="700" font-size="26" fill="var(--muted)">?</text></svg>`;
}
function renderModDetails(it, i) {
  const f = it.info, c = it.copy, g = modGame(it), P = modsView.pick;
  const modes = ["plugins", "content", "as_is"].filter((m) => m !== "content" || f?.kind === "stfs");
  const gameLine = !g ? `<span class="warn-text">Which game is this for?</span>`
    : `${escapeHtml(g.name)} <span class="meta">${escapeHtml(g.tid)}</span> <span class="pill ${g.how === "guess" ? "pill-soft" : ""}">${g.how === "header" ? "from header" : g.how === "you" ? "your choice" : g.how === "builtin" ? "built-in list" : `guess ${g.conf}% · ${escapeHtml(g.why || "")}`}</span>`;
  const sugg = !f?.title_id && (!g || P) ? `<div class="mod-pick">
      ${(f?.guesses || []).length ? `<div class="meta">Best matches:</div><div class="chips">${f.guesses.map((x, k) => `<button class="chip" data-mod-guess="${k}" type="button" title="${escapeHtml(x.reason)}">${escapeHtml(x.name)} <span>${x.confidence}%</span></button>`).join("")}</div>` : ""}
      <input data-mod-gsearch value="${escapeHtml(P?.q || "")}" placeholder="Search games (name, alias like BO2, or TitleID)" aria-label="Search games" />
      ${P?.results ? `<div class="chips">${P.results.map((x, k) => `<button class="chip" data-mod-gres="${k}" type="button">${escapeHtml(x.name)} <span>${escapeHtml(x.title_id)}</span></button>`).join("") || `<span class="meta">No match.</span>`}</div>` : ""}
      ${g ? `<button class="btn btn-quiet btn-sm" data-mod-gcancel type="button">Cancel</button>` : ""}</div>` : "";
  return `<aside class="mod-details" data-mod-details="${i}">
    <div class="md-head">${modTile(it)}<div><strong title="${escapeHtml(it.path)}">${escapeHtml(it.builtin?.name || f?.name || N.baseName(it.path))}</strong><span class="meta">${escapeHtml(f?.label || "")} · ${formatBytes(f?.size || 0)}${f?.kind === "folder" ? ` · ${f.files} files` : ""}</span></div></div>
    ${it.error ? `<div class="error-box danger">${escapeHtml(it.error)}</div>` : ""}
    <div class="md-row"><span class="meta">Game</span><div>${gameLine} ${g && !f?.title_id && !P && !it.builtin ? `<button class="btn btn-quiet btn-sm" data-mod-gchange type="button">Change…</button>` : ""}</div></div>
    ${sugg}
    <div class="md-row"><span class="meta">Location</span><span class="md-path" title="${escapeHtml(it.path)}">${escapeHtml(it.path)}</span></div>
    <label class="md-row"><span class="meta">Notes</span>${it.builtin ? `<span>Built-in (read-only) · version ${escapeHtml(it.builtin.version || "?")}${it.builtin.sha256 ? ` · SHA-256 checked` : ""}</span>` : `<textarea class="mod-notes" data-mod-notes="${i}" rows="2" maxlength="500" placeholder="Version, what it's for…">${escapeHtml(it.notes)}</textarea>`}</label>
    <label class="md-row"><span class="meta">Copies to</span><select data-mod-mode="${i}" ${it.error ? "disabled" : ""}>${modes.map((m) => `<option value="${m}" ${it.mode === m ? "selected" : ""}>${MODE_LABEL[m]}</option>`).join("")}</select></label>
    ${c ? `<div class="qbar"><div data-job-bar="${escapeHtml(c.id)}" class="qbar-fill"></div></div><div class="${c.error ? "danger" : "meta"}" data-mod-status data-job-status="${escapeHtml(c.id)}">${escapeHtml(c.error || c.status)}</div>` : ""}
    <div class="actions"><button class="btn btn-primary btn-sm" data-mod-copy="${i}" type="button" title="Copy to a USB drive or folder (checks free space, never overwrites)" ${it.error || (c && !c.done && !c.error) ? "disabled" : ""}>${icon("out")} Copy to USB…</button><button class="btn btn-secondary btn-sm" data-mod-xbox="${i}" type="button" title="Send to your Xbox over WiFi / LAN (FTP)" ${it.error ? "disabled" : ""}>${SV(16, 16, ICONS.xbox)} Send to Xbox</button><button class="btn btn-quiet btn-sm" data-mod-show="${i}" type="button">Show</button>${it.builtin ? "" : `<button class="btn btn-quiet btn-sm" data-mod-remove="${i}" type="button" title="Remove from this list (the file itself is not touched)">Remove</button>`}</div>
  </aside>`;
}
function renderMods() {
  if (!modsView) return "";
  const M = modsView, vis = modsVisible();
  const games = new Map(); let unknown = 0;
  M.items.forEach((it) => { const g = modGame(it); g ? games.set(g.tid, g.name) : unknown++; });
  if (!vis.some(([, i]) => i === M.sel)) M.sel = vis[0]?.[1] ?? -1;
  const grid = M.loading ? `<div class="preview-empty"><span class="spin">◌</span> Reading headers…</div>`
    : !M.items.length ? `<div class="preview-empty lib-empty">${EMPTY_MODS_ART}<div><strong>No mods added yet</strong><br>Add .xex plugins, CON/LIVE/PIRS packages or folders you already have on this PC.</div></div>`
    : !vis.length ? `<div class="preview-empty">Nothing matches.</div>`
    : `<div class="mod-grid">${vis.map(([it, i]) => { const g = modGame(it); return `<button class="mod-card${i === M.sel ? " sel" : ""}" data-mod-card="${i}" type="button" title="${escapeHtml(it.path)}">
        ${modTile(it)}<strong>${escapeHtml(it.builtin?.name || it.info?.name || N.baseName(it.path))}</strong>
        <span class="meta">${g ? escapeHtml(g.name) : `<span class="warn-text">Which game?</span>`}</span>
        <span><span class="pill">${escapeHtml(TYPE_LABEL[it.info?.kind] || "?")}</span>${g?.how === "guess" ? ` <span class="pill pill-soft">${g.conf}%</span>` : ""}${it.builtin ? ` <span class="pill" data-builtin-pill>Built-in</span>` : ""}${it.copy?.done ? ` <span class="pill ok">copied</span>` : ""}</span></button>`; }).join("")}</div>`;
  return `<div class="overlay" data-mods-overlay><div class="dialog dialog-xl mods-dialog" data-mods role="dialog" aria-modal="true" aria-labelledby="mods-h">
    <h3 id="mods-h">${SV(18, 18, ICONS.mod)} Mod Menus <span class="meta meta-h">${M.items.length ? M.items.length + " item" + (M.items.length === 1 ? "" : "s") : ""}</span></h3>
    <p class="mods-note" role="note">For offline / modded-console use. Unpack doesn't include or link to any mods and ships no catalog. It only goes online when you click Download, or Refresh / Install for a catalog you set up — the page itself stays offline.</p>
    <div class="mods-tabs"><button class="btn btn-sm ${M.tab === "library" || !M.tab ? "btn-secondary" : "btn-quiet"}" data-mods-tab="library" type="button">Your files</button><button class="btn btn-sm ${M.tab === "download" ? "btn-secondary" : "btn-quiet"}" data-mods-tab="download" type="button">Download from link</button><button class="btn btn-sm ${M.tab === "names" ? "btn-secondary" : "btn-quiet"}" data-mods-tab="names" type="button">Known names${(M.names || []).length ? ` (${M.names.length})` : ""}</button><button class="btn btn-sm ${M.tab === "catalog" ? "btn-secondary" : "btn-quiet"}" data-mods-tab="catalog" type="button">Catalog${catalogView?.updates ? ` <span class="pill" data-cat-upd-count>${catalogView.updates} update${catalogView.updates === 1 ? "" : "s"}</span>` : ""}</button></div>
    ${M.tab === "download" ? renderModsDownload(M) : ""}${M.tab === "names" ? renderModNames(M) : ""}${M.tab === "catalog" ? renderCatalog() : ""}
    <div class="mods-bar${M.tab === "download" || M.tab === "names" || M.tab === "catalog" ? " is-hidden" : ""}">
      <input data-mods-q type="search" value="${escapeHtml(M.q)}" placeholder="Search your mods…" aria-label="Search your mods" />
      <select data-mods-fgame aria-label="Filter by game"><option value="">All games</option>${[...games].sort((a, b) => a[1].localeCompare(b[1])).map(([t, n]) => `<option value="${escapeHtml(t)}" ${M.fGame === t ? "selected" : ""}>${escapeHtml(n)}</option>`).join("")}${unknown ? `<option value="?" ${M.fGame === "?" ? "selected" : ""}>Game unknown (${unknown})</option>` : ""}</select>
      <select data-mods-ftype aria-label="Filter by type"><option value="">All types</option>${Object.entries(TYPE_LABEL).map(([k, v]) => `<option value="${k}" ${M.fType === k ? "selected" : ""}>${v}</option>`).join("")}</select>
      <button class="btn btn-secondary btn-sm" data-mods-add-files type="button" title="Add .xex plugins or packages">${SV(16, 16, ICONS.plus)} Add files…</button><button class="btn btn-secondary btn-sm" data-mods-add-folder type="button" title="Add a whole mod folder">${icon("folder")} Add folder…</button>
    </div>
    <div class="mods-body${M.tab === "download" || M.tab === "names" || M.tab === "catalog" ? " is-hidden" : ""}">${grid}${M.sel >= 0 && M.items[M.sel] && !M.loading ? renderModDetails(M.items[M.sel], M.sel) : ""}</div>
    <div class="actions actions-end-mt"><button class="btn btn-primary" data-mods-close type="button">Done</button></div>
  </div></div>`;
}
const EMPTY_MODS_ART = `<svg viewBox="0 0 120 80" width="120" height="80" aria-hidden="true"><rect x="30" y="14" width="60" height="44" rx="8" fill="var(--elevated)" stroke="var(--border-strong)"/><path d="M40 28h22M40 36h30M40 44h16" stroke="var(--border-strong)" stroke-width="2.5" stroke-linecap="round"/><circle cx="84" cy="54" r="13" fill="var(--card)" stroke="var(--accent2)" stroke-width="2"/><path d="M84 48v12M78 54h12" stroke="var(--accent2)" stroke-width="2.5" stroke-linecap="round"/><ellipse cx="60" cy="74" rx="40" ry="3" fill="var(--border)"/></svg>`;
async function copyModToDrive(it) {
  const hint = it.mode === "content" ? "It goes into Content\\0000000000000000\\<TitleID>." : it.mode === "plugins" ? "It goes into a Plugins folder there." : "It's copied there as is.";
  const target = await N.pickFolder("Pick the USB drive or folder (its top). " + hint);
  if (!target) return;
  const c = (it.copy = { id: uid(), progress: 0, status: "Checking space…", error: "", done: false });
  render();
  const off = N.onJobProgress(c.id, (p) => { c.progress = p.percent; c.status = `Copying ${p.percent}% · ${p.file}`; paintJob(c); });
  try {
    const r = await N.invoke("mod_copy", { id: c.id, path: it.path, target, mode: it.mode });
    c.progress = 100; c.done = true;
    c.status = r.copied ? `Copied ${r.copied} file${r.copied === 1 ? "" : "s"} (${formatBytes(r.bytes)})${r.identical ? `, ${r.identical} already there` : ""} → ${r.dest}` : `Already there, identical → ${r.dest}`;
    addHistory({ name: it.info?.name || N.baseName(it.path), source: it.path, output: r.dest, result: "created", label: "Mod copied" });
  } catch (err) { c.error = friendlyError(err); } finally { off(); render(); }
}
function setModGame(it, tid, name) { it.tid = tid; it.gname = name; modsView.pick = null; saveMods(); render(); if (!it.info?.title_id) learnModName(it.info?.name || N.baseName(it.path), tid); }
/** Remember "normalized mod name -> TitleID" so later versions of the same mod are recognised. */
async function learnModName(name, tid) {
  const stem = String(name).replace(/\.(xex|zip|rar|7z|tar|gz)$/i, "");
  let r;
  try { r = await N.invoke("mod_name_key", { name: stem, tid }); } catch { return null; }
  const list = N.settings.modNames.filter((x) => x.key !== r.key);
  list.unshift({ key: r.key, tid: r.tid, gname: r.game });
  N.settings.modNames = list.slice(0, 500);
  if (modsView) modsView.names = N.settings.modNames;
  return r;
}
function renderModNames(M) {
  const L = M.names || [], P = M.namePick;
  return `<div class="mods-dl" data-modnames>
    <p class="meta">Mod names you've confirmed a game for. When a new file, folder or archive name contains one of these, Unpack picks that game first (a TitleID in the file's header still wins). Version numbers, “menu” words and the game's own name are left out. Unpack ships none of these.</p>
    <div class="actions"><button class="btn btn-quiet btn-sm" data-mn-export type="button">Export list…</button><button class="btn btn-quiet btn-sm" data-mn-import type="button">Import list…</button></div>
    <div class="mod-list">${L.map((x, i) => `<div class="mod-row" data-mn-row="${i}"><div class="mod-info"><input data-mn-key="${i}" value="${escapeHtml(x.key)}" maxlength="80" aria-label="Mod name" /><span class="meta">${escapeHtml(x.gname || "")} · ${escapeHtml(x.tid)}</span>
      ${P && P.i === i ? `<input data-mn-gsearch value="${escapeHtml(P.q || "")}" placeholder="Search games" aria-label="Search games" />${P.results ? `<div class="chips">${P.results.map((g, k) => `<button class="chip" data-mn-gres="${k}" type="button">${escapeHtml(g.name)} <span>${escapeHtml(g.title_id)}</span></button>`).join("") || `<span class="meta">No match.</span>`}</div>` : ""}` : ""}</div>
      <div class="actions"><button class="btn btn-quiet btn-sm" data-mn-remap="${i}" type="button">Change game…</button><button class="btn btn-quiet btn-sm" data-mn-del="${i}" type="button">Delete</button></div></div>`).join("") || `<div class="preview-empty preview-empty-short">No known mod names yet. Pick a game for a mod in “Your files” and it's remembered here.</div>`}</div></div>`;
}
let exportFmtPrompt = null; // { resolve }
function askExportFormat() {
  return new Promise((resolve) => { exportFmtPrompt = { resolve }; render(); });
}
function renderExportFmt() {
  if (!exportFmtPrompt) return "";
  return `<div class="overlay" data-fmt-overlay><div class="dialog" role="dialog" aria-modal="true"><h3>Export format</h3>
    <div class="fmt-pick"><button class="btn btn-secondary" data-fmt-json type="button">JSON</button><button class="btn btn-secondary" data-fmt-csv type="button">CSV</button><button class="btn btn-quiet" data-fmt-cancel type="button">Cancel</button></div>
  </div></div>`;
}
function bindExportFmt() {
  const root = app.querySelector("[data-fmt-overlay]");
  if (!root || !exportFmtPrompt) return;
  const done = (v) => { exportFmtPrompt.resolve(v); exportFmtPrompt = null; render(); };
  root.querySelector("[data-fmt-json]")?.addEventListener("click", () => done("json"));
  root.querySelector("[data-fmt-csv]")?.addEventListener("click", () => done("csv"));
  root.querySelector("[data-fmt-cancel]")?.addEventListener("click", () => done(null));
  root.addEventListener("click", (e) => { if (e.target === e.currentTarget) done(null); });
}
function renderDlcPick() {
  if (!dlcPick) return "";
  const P = dlcPick;
  return `<div class="overlay" data-dlc-pick-overlay><div class="dialog" role="dialog" aria-modal="true"><h3>Which game gets this package?</h3>
    <p class="meta">${escapeHtml(P.row.status || "")}</p>
    <div class="mod-list">${P.games.map((g, i) => `<button class="chip" data-dlc-pick="${i}" type="button">${escapeHtml(g.name)} <span>${escapeHtml(N.baseName(g.path))}</span></button>`).join("") || `<span class="meta">No matching games in your library folders.</span>`}</div>
    <div class="actions actions-end-mt"><button class="btn btn-quiet" data-dlc-pick-skip type="button">Skip</button></div>
  </div></div>`;
}
function bindDlcPick() {
  const root = app.querySelector("[data-dlc-pick-overlay]");
  if (!root || !dlcPick) return;
  const finish = (path) => {
    if (path) dlcPick.row.gamePath = path;
    dlcPick.resolve();
    dlcPick = null;
    render();
  };
  root.querySelectorAll("[data-dlc-pick]").forEach((b) => b.addEventListener("click", () => finish(dlcPick.games[+b.dataset.dlcPick].path)));
  root.querySelector("[data-dlc-pick-skip]")?.addEventListener("click", () => finish(null));
}
async function exportModNames(list) {
  const fmt = await askExportFormat();
  if (!fmt) return;
  const rows = (list || []).map((x) => ({ key: String(x.key).slice(0, 80), tid: x.tid, game: String(x.gname || "").slice(0, 120) }));
  const csv = /^csv$/i.test(fmt.trim());
  const body = csv ? ["key,tid,game", ...rows.map((r) => [r.key, r.tid, r.game].map(csvSafeCell).join(","))].join("\n") : JSON.stringify(rows, null, 2);
  const ext = csv ? ".csv" : ".json";
  const out = await N.pickSave(`unpack-mod-names${ext}`, [{ name: csv ? "CSV" : "JSON", extensions: [csv ? "csv" : "json"] }], "Export mod names");
  if (!out) return;
  window.__exportPickSaveCalled = true;
  try { await N.saveText(out, body); } catch (e) { toast("Couldn't save: " + (e?.message || e)); return; }
  toast("Exported " + rows.length + " names to " + N.baseName(out));
}
function parseCsvLine(line) {
  const cols = []; let cur = "", q = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === '"') { if (q && line[i + 1] === '"') { cur += '"'; i++; } else q = !q; }
    else if (c === "," && !q) { cols.push(cur); cur = ""; } else cur += c;
  }
  cols.push(cur);
  return cols.map((c) => c.replace(/^'(?=[=+\-@\t\r])/, ""));
}
/** Rows from a JSON or CSV text: [{key, tid}], length-capped. Validation happens in Rust. */
function parseModNamesText(text, isCsv) {
  let rows = [];
  if (isCsv) {
    const lines = text.replace(/^\uFEFF/, "").split(/\r?\n/).filter((l) => l.trim());
    const hdr = parseCsvLine(lines.shift() || "").map((h) => h.trim().toLowerCase());
    const ki = hdr.indexOf("key") >= 0 ? hdr.indexOf("key") : hdr.indexOf("name"), ti = hdr.indexOf("tid") >= 0 ? hdr.indexOf("tid") : hdr.indexOf("titleid");
    if (ki < 0 || ti < 0) throw new Error("CSV needs key and tid columns");
    rows = lines.map((l) => { const c = parseCsvLine(l); return { key: c[ki], tid: c[ti] }; });
  } else {
    const raw = JSON.parse(text);
    rows = (Array.isArray(raw) ? raw : raw.names || []).map((x) => ({ key: x && (x.key ?? x.name), tid: x && (x.tid ?? x.title_id) }));
  }
  const seen = new Set();
  // TitleID must be exactly 8 hex digits as given (never cut a longer value down to 8).
  return rows.map((r) => ({ key: String(r.key ?? "").slice(0, 200).trim(), tid: String(r.tid ?? "").trim() }))
    .filter((r) => r.key && /^[0-9A-Fa-f]{8}$/.test(r.tid))
    .map((r) => ({ ...r, tid: r.tid.toUpperCase() }))
    .filter((r) => { const k = r.key.toLowerCase(); if (seen.has(k)) return false; seen.add(k); return true; })
    .slice(0, 500);
}
async function importModNamesText(text, isCsv) {
  let rows;
  try { rows = parseModNamesText(text, isCsv); } catch (e) { toast("Could not parse list: " + e.message); return { added: 0, bad: 0 }; }
  const list = N.settings.modNames, have = new Set(list.map((x) => x.key));
  let added = 0, bad = 0;
  for (const r of rows) {
    let k;
    try { k = await N.invoke("mod_name_key", { name: r.key, tid: r.tid }); } catch { bad++; continue; }
    if (have.has(k.key)) continue;
    have.add(k.key); list.push({ key: k.key, tid: k.tid, gname: k.game }); added++;
  }
  N.settings.modNames = list.slice(0, 500);
  if (modsView) modsView.names = N.settings.modNames;
  toast(`Imported ${added} name${added === 1 ? "" : "s"}${bad ? `, ${bad} skipped (unknown TitleID or nothing distinctive)` : ""}.`);
  render();
  return { added, bad };
}
window.__unpackModNames = { importText: importModNamesText, parse: parseModNamesText };
let gsearchTimer = 0;


function csvSafeCell(x) {
  let t = String(x ?? "");
  // Formula injection: Excel/Sheets treat leading = + - @ as formulas (also tab/CR).
  if (/^[=+\-@\t\r]/.test(t)) t = "'" + t;
  return `"${t.replace(/"/g, '""')}"`;
}
function normalizeLinkUrl(url) {
  const u = String(url || "").trim();
  if (!/^https?:\/\//i.test(u)) return null;
  try {
    const x = new URL(u);
    if (x.protocol !== "http:" && x.protocol !== "https:") return null;
    return x.href;
  } catch { return null; }
}
async function exportSavedLinks(links) {
  const fmt = await askExportFormat();
  if (!fmt) return;
  const rows = (links || []).map((l) => ({
    name: String(l.name || "").slice(0, 120),
    url: String(l.url || "").slice(0, 2000),
    notes: String(l.notes || "").slice(0, 500),
    game: String(l.game || "").slice(0, 120),
  }));
  let body, name, mime;
  if (/^csv$/i.test(fmt.trim())) {
    body = ["name,url,notes,game", ...rows.map((r) => [r.name, r.url, r.notes, r.game].map(csvSafeCell).join(","))].join("\n");
    name = "unpack-links.csv"; mime = "text/csv";
  } else {
    body = JSON.stringify(rows, null, 2);
    name = "unpack-links.json"; mime = "application/json";
  }
  const out = await N.pickSave(name, [{ name: /^csv$/i.test(fmt.trim()) ? "CSV" : "JSON", extensions: [/^csv$/i.test(fmt.trim()) ? "csv" : "json"] }], "Export saved links");
  if (!out) return;
  window.__exportPickSaveCalled = true;
  try { await N.saveText(out, body); } catch (e) { toast("Couldn't save: " + (e?.message || e)); return; }
  toast("Exported " + rows.length + " links.");
}
async function importSavedLinks(M) {
  const input = document.createElement("input");
  input.type = "file"; input.accept = ".json,.csv,text/csv,application/json";
  input.onchange = async () => {
    const f = input.files?.[0]; if (!f) return;
    if (f.size > 2 * 1024 * 1024) return toast("That list file is too large (max 2 MB).");
    const text = await f.text();
    let rows = [];
    try {
      if (/\.csv$/i.test(f.name) || /^\s*name\s*,/i.test(text)) {
        const lines = text.replace(/^\uFEFF/, "").split(/\r?\n/).filter((l) => l.trim());
        const hdr = lines.shift().split(",").map((h) => h.trim().replace(/^"|"$/g, "").toLowerCase());
        const ix = (k) => hdr.indexOf(k);
        for (const line of lines) {
          const cols = []; let cur = "", q = false;
          for (let i = 0; i < line.length; i++) {
            const c = line[i];
            if (c === '"') { if (q && line[i+1] === '"') { cur += '"'; i++; } else q = !q; }
            else if (c === "," && !q) { cols.push(cur); cur = ""; }
            else cur += c;
          }
          cols.push(cur);
          const url = normalizeLinkUrl(cols[ix("url")] || "");
          if (!url) continue;
          rows.push({
            id: uid(),
            name: String(cols[ix("name")] || url).slice(0, 120),
            url,
            notes: String(cols[ix("notes")] || "").slice(0, 500),
            game: String(cols[ix("game")] || "").slice(0, 120),
            sha: "",
          });
        }
      } else {
        const raw = JSON.parse(text);
        const arr = Array.isArray(raw) ? raw : (raw.links || []);
        rows = arr.map((x) => {
          const url = normalizeLinkUrl(x && x.url);
          if (!url) return null;
          return {
            id: uid(),
            name: String(x.name || url).slice(0, 120),
            url,
            notes: String(x.notes || "").slice(0, 500),
            game: String(x.game || "").slice(0, 120),
            sha: String(x.sha || "").slice(0, 64),
          };
        }).filter(Boolean);
      }
    } catch (e) { return toast("Could not parse list: " + e); }
    // Dedupe within the file (keep first), then against existing.
    const seenFile = new Set();
    rows = rows.filter((r) => { if (seenFile.has(r.url)) return false; seenFile.add(r.url); return true; });
    if (!rows.length) return toast("No http(s) links found in that file.");
    const have = new Set(M.links.map((l) => l.url));
    const fresh = rows.filter((r) => !have.has(r.url));
    M.links = [...fresh, ...M.links].slice(0, 200);
    N.settings.savedLinks = M.links;
    toast(`Imported ${fresh.length} link${fresh.length === 1 ? "" : "s"} (${rows.length - fresh.length} already saved).`);
    render();
  };
  input.click();
}


// ---- Mod catalog: a signed manifest the user points Unpack at. Unpack ships none. ----
let catalogView = null; // { cfg, loaded, states, target, busy, error, q, pf: {}, run: {}, confirm: {} }
const CAT_ART = `<svg viewBox="0 0 120 80" width="120" height="80" aria-hidden="true"><rect x="22" y="12" width="76" height="52" rx="8" fill="var(--elevated)" stroke="var(--border-strong)"/><path d="M34 28h30M34 38h44M34 48h22" stroke="var(--border-strong)" stroke-width="2.5" stroke-linecap="round"/><circle cx="86" cy="52" r="12" fill="var(--card)" stroke="var(--accent)" stroke-width="2"/><path d="M80 52l4 4 8-8" fill="none" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/><ellipse cx="60" cy="74" rx="40" ry="3" fill="var(--border)"/></svg>`;
const catCall = (cmd, args = {}, id) => N.invoke("catalog", { cmd, args, id: id || null });
function catTarget() {
  const C = catalogView, t = C.target;
  if (t.kind === "console") { const k = N.settings.consoles[t.console] || null; return k ? { kind: "console", host: k.host, port: k.port, user: k.user, pass: k.pass, drive: t.drive || pickDrive(catalogView?.drives?.[consoleKey(k)] || [], rememberedDrive(k)) } : null; }
  return t.path ? { kind: "local", path: t.path } : null;
}
async function openCatalog() {
  if (!catalogView) {
    catalogView = { cfg: null, loaded: null, states: {}, target: { kind: "local", path: "", console: 0, drive: "" }, drives: {}, drivesBusy: false, drivesError: "", busy: false, error: "", q: "", pf: {}, run: {}, confirm: {}, updates: 0, edit: false };
    try { catalogView.cfg = await catCall("config_get"); catalogView.loaded = await catCall("current"); } catch (e) { catalogView.error = String(e); }
    catalogView.edit = !catalogView.cfg?.source;
  }
  if (catalogView.cfg?.source) {
    if (!catalogView.loaded) await catRefresh();
    else { await catStates(); render(); }
  } else render();
}
async function catRefresh() {
  const C = catalogView; C.busy = true; C.error = ""; render();
  try { C.loaded = await catCall("refresh"); await catStates(); toast(`Catalog loaded: ${C.loaded.entries.length} entr${C.loaded.entries.length === 1 ? "y" : "ies"}`); }
  catch (e) { C.error = String(e); } finally { C.busy = false; render(); }
}
async function catStates() {
  const C = catalogView, t = catTarget();
  C.states = {}; C.updates = 0; C.pf = {};
  if (!C.loaded || !t) return;
  try { for (const s of await catCall("states", { target: t })) C.states[s.id] = s; C.updates = Object.values(C.states).filter((s) => s.state === "update").length; }
  catch (e) { C.error = String(e); }
}
async function catSaveConfig() {
  const C = catalogView, f = C.form;
  try { C.cfg = await catCall("config_set", { config: f }); C.edit = false; C.error = ""; await catRefresh(); }
  catch (e) { C.error = String(e); render(); }
}
async function catPreflight(id) {
  const C = catalogView, t = catTarget();
  if (!t) { C.error = "Pick where to install first (a drive on this PC, or a saved console)."; render(); return; }
  C.pf[id] = { busy: true }; render();
  try { C.pf[id] = await catCall("preflight", { id, target: t }); } catch (e) { C.pf[id] = { blockers: [String(e)], warnings: [], info: [] }; }
  render();
}
// Ask the chosen console for its drives (catalog console target).
async function catListDrives() {
  const C = catalogView, k = N.settings.consoles[C.target.console];
  if (!k) return;
  C.drivesBusy = true; C.drivesError = ""; render();
  try {
    const list = rootDrives(await N.invoke("ftp_list", { host: k.host, port: k.port, user: k.user, pass: k.pass, path: "/" }));
    C.drives = { ...C.drives, [consoleKey(k)]: list };
    if (!list.length) C.drivesError = "The console listed no drives.";
    else if (!list.includes(C.target.drive)) C.target.drive = "";
  } catch (err) { C.drivesError = xboxError(err); }
  finally { C.drivesBusy = false; }
}
async function catInstall(id, opts = {}) {
  const C = catalogView, t = catTarget();
  if (!t) return;
  const jid = uid();
  C.run[id] = { id: jid, progress: 0, status: "Starting…", error: "" }; delete C.confirm[id]; render();
  const off = N.onJobProgress(jid, (p) => { const r = C.run[id]; if (!r) return; r.progress = p.percent; r.status = `${p.file} ${p.percent}%`; paintJob(r); });
  try {
    const r = await catCall("install", { id, target: t, opts }, jid);
    C.run[id] = { id: jid, progress: 100, done: true, status: `Installed ${r.files} file${r.files === 1 ? "" : "s"} (${formatBytes(r.bytes)})${r.updated_from ? ` · updated from ${r.updated_from}` : ""}${r.ini ? ` · launch.ini: ${r.ini}` : ""}${r.notes.length ? " · " + r.notes.join(" ") : ""}` };
    addHistory({ name: C.loaded.entries.find((e) => e.id === id)?.mod.name || id, source: "Catalog", output: catTargetLabel(), result: "created", label: "Installed from catalog" });
  } catch (e) {
    const m = String(e);
    if (m.startsWith("CONFIRM_WARNINGS:")) { C.confirm[id] = { kind: "warn", text: m.slice(17), opts }; delete C.run[id]; }
    else if (m.startsWith("SLOT_OCCUPIED:")) { const [, slot, ...rest] = m.split(":"); C.confirm[id] = { kind: "slot", slot: +slot, text: rest.join(":"), opts }; delete C.run[id]; }
    else C.run[id] = { id: jid, error: m.includes("CANCELLED") ? "Cancelled." : m };
  } finally { off(); await catStates(); render(); }
}
async function catUninstall(id) {
  const C = catalogView, t = catTarget();
  if (!t) return;
  C.run[id] = { id: uid(), status: "Removing…" }; render();
  try { const r = await catCall("uninstall", { id, target: t }); C.run[id] = { id: uid(), done: true, status: `Removed ${r.removed} file${r.removed === 1 ? "" : "s"}${r.kept.length ? ` · kept ${r.kept.length} file${r.kept.length === 1 ? "" : "s"} (changed since install or used by another mod): ${r.kept.join(", ")}` : ""}${r.ini ? ` · launch.ini: ${r.ini}` : ""}${(r.notes || []).length ? " · " + r.notes.join(" ") : ""}` }; }
  catch (e) { C.run[id] = { id: uid(), error: String(e) }; } finally { await catStates(); render(); }
}
function catTargetLabel() { const t = catTarget(); return !t ? "" : t.kind === "local" ? t.path : `${t.host} /${t.drive}`; }
function renderCatalog() {
  const C = catalogView;
  if (!C) return `<div class="preview-empty"><span class="spin">◌</span></div>`;
  const cfg = C.cfg || {}, f = (C.form ||= { source: cfg.source || "remote", url: cfg.url || "", sig_url: cfg.sig_url || "", local_file: cfg.local_file || "", pubkey: cfg.pubkey || "" });
  const consoles = N.settings.consoles;
  const setup = C.edit ? `<div class="mods-dl cat-setup" data-cat-setup>
      <p class="meta">Unpack doesn't include a catalog. If someone runs one, they give you its <b>address</b> and <b>public key</b>. Unpack only loads a catalog whose signature matches that key, and checks every file's SHA-256 before installing.</p>
      <div class="actions"><label class="meta"><input type="radio" name="cat-src" data-cat-src="remote" ${f.source === "remote" ? "checked" : ""}/> From a web address</label><label class="meta"><input type="radio" name="cat-src" data-cat-src="local" ${f.source === "local" ? "checked" : ""}/> From a file on this PC</label></div>
      ${f.source === "local" ? `<div class="actions"><input data-cat-f="local_file" value="${escapeHtml(f.local_file)}" placeholder="catalog.json (its .sig must be next to it)" class="inp-flex1" spellcheck="false" readonly/><button class="btn btn-secondary btn-sm" data-cat-pickfile type="button">Pick file…</button></div>`
        : `<label>Catalog address<input data-cat-f="url" type="url" value="${escapeHtml(f.url)}" placeholder="https://…/catalog.json" spellcheck="false"/></label><label>Signature address (optional)<input data-cat-f="sig_url" type="url" value="${escapeHtml(f.sig_url)}" placeholder="Default: the catalog address + .sig" spellcheck="false"/></label>`}
      <label>Owner's public key (Ed25519)<input data-cat-f="pubkey" value="${escapeHtml(f.pubkey)}" placeholder="64 hex characters" spellcheck="false" maxlength="64"/></label>
      <div class="actions"><button class="btn btn-primary btn-sm" data-cat-save type="button" ${C.busy ? "disabled" : ""}>Save and load</button>${cfg.source ? `<button class="btn btn-quiet btn-sm" data-cat-editcancel type="button">Cancel</button>` : ""}</div></div>`
    : `<div class="cat-src-row"><span class="meta">${cfg.source === "local" ? `File: <b>${escapeHtml(cfg.local_file)}</b>` : `From <b>${escapeHtml(cfg.url)}</b>`} · key <code>${escapeHtml((cfg.pubkey || "").slice(0, 12))}…</code>${C.loaded ? ` · version ${C.loaded.catalog_version}` : ""}</span>
      <span class="actions"><button class="btn btn-secondary btn-sm" data-cat-refresh type="button" ${C.busy ? "disabled" : ""}>${C.busy ? `<span class="spin">◌</span> ` : ""}Refresh</button><button class="btn btn-quiet btn-sm" data-cat-edit type="button">Change source…</button></span></div>`;
  const t = C.target;
  const target = `<div class="cat-target"><span class="meta">Install to</span>
      <select data-cat-tkind aria-label="Install target"><option value="local" ${t.kind === "local" ? "selected" : ""}>A drive or folder on this PC</option><option value="console" ${t.kind === "console" ? "selected" : ""} ${consoles.length ? "" : "disabled"}>The console (Send to Xbox)${consoles.length ? "" : " — save a console first"}</option></select>
      ${t.kind === "local" ? `<span class="meta" data-cat-tpath>${escapeHtml(t.path || "Nothing picked")}</span><button class="btn btn-secondary btn-sm" data-cat-pickdrive type="button">${icon("folder")} Pick drive…</button>`
        : `<select data-cat-tconsole aria-label="Console">${consoles.map((k, j) => `<option value="${j}" ${t.console === j ? "selected" : ""}>${escapeHtml(k.name)} · ${escapeHtml(k.host)}</option>`).join("")}</select>${(() => { const k = consoles[t.console]; const list = C.drives[consoleKey(k)] || []; const cur = t.drive || pickDrive(list, rememberedDrive(k)); const opts = list.length ? list : [cur];
          return `<select data-cat-tdrive aria-label="Drive">${opts.map((d) => `<option ${cur === d ? "selected" : ""}>${escapeHtml(d)}</option>`).join("")}</select><button class="btn btn-quiet btn-sm" data-cat-listdrives type="button" ${C.drivesBusy ? "disabled" : ""} title="Ask the console which drives it has">${C.drivesBusy ? `<span class="spin">◌</span> ` : ""}${list.length ? "Refresh drives" : "List drives"}</button>${C.drivesError ? `<span class="danger" data-cat-driveserr>${escapeHtml(C.drivesError)}</span>` : ""}`; })()}`}</div>`;
  const L = C.loaded;
  const q = C.q.trim().toLowerCase();
  const list = !L ? (cfg.source ? "" : `<div class="preview-empty lib-empty">${CAT_ART}<div><strong>No catalog set up</strong><br>Nothing is bundled. Add a catalog's address and its owner's public key above.</div></div>`)
    : !L.entries.length ? `<div class="preview-empty" data-cat-empty>This catalog has no entries.</div>`
    : `<div class="cat-list" data-cat-list>${L.entries.filter((e) => !q || [e.mod.name, e.mod.author, e.game.name, ...(e.game.aliases || [])].some((x) => (x || "").toLowerCase().includes(q))).map((e) => {
        const st = C.states[e.id] || { state: "new" }, pf = C.pf[e.id], run = C.run[e.id], cf = C.confirm[e.id];
        const only = e.paid || !e.redistribute || !e.files.length;
        const size = e.files.reduce((a, x) => a + x.size, 0);
        const badge = st.state === "installed" ? `<span class="pill ok" data-cat-badge="installed">Installed ${escapeHtml(st.installed_version || "")}</span>` : st.state === "update" ? `<span class="pill" data-cat-badge="update">Update available: ${escapeHtml(st.installed_version || "")} → ${escapeHtml(e.mod.version)}</span>` : st.state === "partial" ? `<span class="pill warn-text" data-cat-badge="partial">Partly installed</span>` : "";
        const busy = run && !run.done && !run.error;
        return `<div class="mod-row cat-row" data-cat-entry="${escapeHtml(e.id)}"><div class="mod-info"><strong>${escapeHtml(e.mod.name)}</strong> <span class="meta">v${escapeHtml(e.mod.version)}${e.mod.author ? ` · ${escapeHtml(e.mod.author)}` : ""}</span>
          <span class="meta">${escapeHtml(e.game.name)} · ${escapeHtml(e.game.title_id)}${e.compat.tu?.length ? ` · ${e.compat.tu.map((n) => (n ? "TU" + n : "no TU")).join("/")}` : ""}${size ? ` · ${formatBytes(size)}` : ""}</span>
          <span>${e.paid ? `<span class="pill" data-cat-paid>Paid</span> ` : only ? `<span class="pill pill-soft">Source page only</span> ` : ""}${e.files.map((x) => `<span class="pill pill-soft">${escapeHtml(x.install.handler)}</span>`).join(" ")} ${badge}</span>
          ${e.notes ? `<span class="meta">${escapeHtml(e.notes)}</span>` : ""}
          ${pf && !pf.busy ? `<div class="cat-pf" data-cat-pf="${escapeHtml(e.id)}">${pf.blockers.map((b) => `<div class="danger">✕ ${escapeHtml(b)}</div>`).join("")}${pf.warnings.map((w) => `<div class="warn-text">! ${escapeHtml(w)}</div>`).join("")}${pf.info.map((i) => `<div class="meta">${escapeHtml(i)}</div>`).join("")}${pf.ok && !pf.warnings.length ? `<div class="meta ok">✓ Ready to install${pf.free != null ? ` (${formatBytes(pf.free)} free)` : ""}</div>` : ""}</div>` : pf?.busy ? `<div class="meta"><span class="spin">◌</span> Checking…</div>` : ""}
          ${cf ? `<div class="cat-confirm" data-cat-confirm="${escapeHtml(e.id)}"><div class="warn-text">${escapeHtml(cf.text)}</div><div class="actions">${cf.kind === "slot" ? `<button class="btn btn-primary btn-sm" data-cat-yes="${escapeHtml(e.id)}" type="button">Replace plugin${cf.slot}</button>` : `<button class="btn btn-primary btn-sm" data-cat-yes="${escapeHtml(e.id)}" type="button">Install anyway</button>`}<button class="btn btn-quiet btn-sm" data-cat-no="${escapeHtml(e.id)}" type="button">Cancel</button></div></div>` : ""}
          ${run ? `${busy ? `<div class="qbar"><div data-job-bar="${escapeHtml(run.id)}" class="qbar-fill"></div></div>` : ""}<div class="${run.error ? "danger" : "meta"}" data-job-status="${escapeHtml(run.id)}" data-cat-status="${escapeHtml(e.id)}">${escapeHtml(run.error || run.status || "")}</div>` : ""}</div>
          <div class="mod-act"><div class="actions">
            ${only ? (e.source_page ? `<button class="btn btn-secondary btn-sm" data-cat-page="${escapeHtml(e.id)}" type="button">Open source page</button>` : "")
              : `${busy ? `<button class="btn btn-quiet btn-sm" data-cat-cancel="${escapeHtml(e.id)}" type="button">Cancel</button>` : `<button class="btn btn-quiet btn-sm" data-cat-check="${escapeHtml(e.id)}" type="button" ${catTarget() ? "" : "disabled"}>Check</button>
                 ${st.state !== "installed" ? `<button class="btn btn-primary btn-sm" data-cat-install="${escapeHtml(e.id)}" type="button" ${catTarget() ? "" : "disabled"}>${st.state === "update" ? "Update" : "Install"}</button>` : ""}
                 ${st.state !== "new" ? `<button class="btn btn-quiet btn-sm" data-cat-uninstall="${escapeHtml(e.id)}" type="button">Uninstall</button>` : ""}`}
               ${e.source_page ? `<button class="btn btn-quiet btn-sm" data-cat-page="${escapeHtml(e.id)}" type="button" title="Open the mod's page">Page</button>` : ""}`}
          </div></div></div>`; }).join("")}</div>`;
  return `<div class="mods-dl" data-catalog>
    ${setup}
    ${C.error ? `<div class="error-box danger" data-cat-error>${escapeHtml(C.error)}</div>` : ""}
    ${L ? target : ""}
    ${L?.entries.length ? `<input data-cat-q type="search" value="${escapeHtml(C.q)}" placeholder="Search the catalog…" aria-label="Search the catalog"/>` : ""}
    ${list}
    ${L?.problems?.length ? `<details class="meta" data-cat-problems><summary>${L.problems.length} entr${L.problems.length === 1 ? "y" : "ies"} skipped (invalid)</summary>${L.problems.map((p) => `<div>${escapeHtml(p)}</div>`).join("")}</details>` : ""}
    <p class="meta">Files go to a staging folder first, are checked (SHA-256, size, no links or paths that leave the folder), then copied. Existing files are never overwritten. launch.ini is backed up before any change, plugin1 is never touched, and a used slot is only replaced if you say so. Uninstall removes only files that are unchanged since install.</p>
  </div>`;
}
function bindCatalog(root) {
  const C = catalogView, el = root.querySelector("[data-catalog]");
  if (!C || !el) return;
  el.querySelectorAll("[data-cat-src]").forEach((r) => r.addEventListener("change", () => { C.form.source = r.dataset.catSrc; render(); }));
  el.querySelectorAll("[data-cat-f]").forEach((i) => i.addEventListener("input", () => { C.form[i.dataset.catF] = i.value; }));
  el.querySelector("[data-cat-pickfile]")?.addEventListener("click", async () => { const [p] = await N.pickFiles("Pick the catalog file (catalog.json)"); if (p) { C.form.local_file = p; render(); } });
  el.querySelector("[data-cat-save]")?.addEventListener("click", catSaveConfig);
  el.querySelector("[data-cat-edit]")?.addEventListener("click", () => { C.edit = true; C.form = null; render(); });
  el.querySelector("[data-cat-editcancel]")?.addEventListener("click", () => { C.edit = false; C.form = null; render(); });
  el.querySelector("[data-cat-refresh]")?.addEventListener("click", catRefresh);
  el.querySelector("[data-cat-q]")?.addEventListener("input", (e) => { C.q = e.target.value; render(); const i = app.querySelector("[data-cat-q]"); if (i) { i.focus(); i.setSelectionRange(C.q.length, C.q.length); } });
  el.querySelector("[data-cat-tkind]")?.addEventListener("change", async (e) => { C.target.kind = e.target.value; C.target.drive = ""; if (C.target.kind === "console") await catListDrives(); await catStates(); render(); });
  el.querySelector("[data-cat-tconsole]")?.addEventListener("change", async (e) => { C.target.console = +e.target.value; C.target.drive = ""; await catListDrives(); await catStates(); render(); });
  el.querySelector("[data-cat-tdrive]")?.addEventListener("change", async (e) => { C.target.drive = e.target.value; rememberDrive(N.settings.consoles[C.target.console], e.target.value); await catStates(); render(); });
  el.querySelector("[data-cat-listdrives]")?.addEventListener("click", async () => { await catListDrives(); await catStates(); render(); });
  el.querySelector("[data-cat-pickdrive]")?.addEventListener("click", async () => { const p = await N.pickFolder("Pick the USB drive (or folder) to install to"); if (p) { C.target.path = p; await catStates(); render(); } });
  const byId = (b, k) => b.dataset[k];
  el.querySelectorAll("[data-cat-check]").forEach((b) => b.addEventListener("click", () => catPreflight(byId(b, "catCheck"))));
  el.querySelectorAll("[data-cat-install]").forEach((b) => b.addEventListener("click", () => catInstall(byId(b, "catInstall"))));
  el.querySelectorAll("[data-cat-uninstall]").forEach((b) => b.addEventListener("click", () => catUninstall(byId(b, "catUninstall"))));
  el.querySelectorAll("[data-cat-cancel]").forEach((b) => b.addEventListener("click", () => { const r = C.run[byId(b, "catCancel")]; if (r) N.invoke("cancel_job", { id: r.id }).catch(() => {}); }));
  el.querySelectorAll("[data-cat-page]").forEach((b) => b.addEventListener("click", () => { const e = C.loaded.entries.find((x) => x.id === byId(b, "catPage")); if (e?.source_page) openSourcePage(e.source_page); }));
  el.querySelectorAll("[data-cat-yes]").forEach((b) => b.addEventListener("click", () => { const id = byId(b, "catYes"), cf = C.confirm[id]; if (!cf) return; catInstall(id, cf.kind === "slot" ? { ...cf.opts, confirmSlot: cf.slot } : { ...cf.opts, acceptWarnings: true }); }));
  el.querySelectorAll("[data-cat-no]").forEach((b) => b.addEventListener("click", () => { delete C.confirm[byId(b, "catNo")]; render(); }));
}
function renderModsDownload(M) {
  const d = M.dl || {};
  const job = d.job;
  const httpWarn = /^http:\/\//i.test(d.url || "") && !/^https:\/\//i.test(d.url || "");
  const hb = d.last && matchHomebrewName(d.last.name);
  return `<div class="mods-dl" data-mods-dl>
    <p class="meta">Paste any http(s) URL. Unpack downloads it in the background (progress, cancel, resume). Direct file links work; public Google Drive / Dropbox / MediaFire pages are rewritten to a direct link when the pattern is simple. No menu catalog is bundled. Default size cap ${N.settings.dlMaxGb} GB (Settings).</p>
    <label>Link<input data-dl-url type="url" value="${escapeHtml(d.url || "")}" placeholder="https://…" spellcheck="false" /></label>
    ${httpWarn ? `<p class="warn-text" role="status">This is plain HTTP (not HTTPS). Prefer an https:// link when you can.</p>` : ""}
    <div class="mods-dl-row"><label>Name (optional)<input data-dl-name value="${escapeHtml(d.name || "")}" placeholder="Shown in your saved list" maxlength="120" /></label>
    <label>Game (optional)<input data-dl-game value="${escapeHtml(d.game || "")}" placeholder="e.g. Black Ops 2" maxlength="120" /></label></div>
    <div class="mods-dl-row"><label>Notes<input data-dl-notes value="${escapeHtml(d.notes || "")}" placeholder="Optional" maxlength="500" /></label>
    <label>Expected SHA-256 (optional)<input data-dl-sha-expect value="${escapeHtml(d.sha || "")}" placeholder="Refuse download if mismatch" maxlength="64" spellcheck="false" /></label></div>
    <div class="actions">
      <button class="btn btn-primary btn-sm" data-dl-go type="button" ${job && !job.done && !job.error ? "disabled" : ""}>Download</button>
      <button class="btn btn-secondary btn-sm" data-dl-save type="button" title="Save this link to re-download later">Save link</button>
      <button class="btn btn-quiet btn-sm" data-dl-folder type="button">Choose folder…</button>
      <button class="btn btn-quiet btn-sm" data-dl-export type="button" title="Export saved links as JSON or CSV">Export list…</button>
      <button class="btn btn-quiet btn-sm" data-dl-import type="button" title="Import saved links from JSON or CSV">Import list…</button>
      ${job && !job.done && !job.error ? `<button class="btn btn-quiet btn-sm" data-dl-cancel type="button">Cancel</button>` : ""}
    </div>
    ${job ? `<div class="qbar"><div data-job-bar="${escapeHtml(job.id)}" class="qbar-fill"></div></div><div class="${job.error ? "danger" : "meta"}" data-job-status="${escapeHtml(job.id)}">${escapeHtml(job.error || job.status || "")}</div>` : ""}
    ${d.last ? `<div class="dl-result"><strong>${escapeHtml(d.last.name)}</strong> · ${formatBytes(d.last.bytes)}<div class="meta">SHA-256: <code data-dl-sha>${escapeHtml(d.last.sha256)}</code></div>
      <div class="actions"><button class="btn btn-secondary btn-sm" data-dl-add-mod type="button">Add to Mod library</button>
      ${hb ? `<button class="btn btn-secondary btn-sm" data-dl-usb-apps type="button" title="Copy into USB\\Apps\\${escapeHtml(hb.name)}">Copy to USB Apps…</button>` : `<button class="btn btn-quiet btn-sm" data-dl-usb-apps type="button">Copy to USB Apps…</button>`}
      <button class="btn btn-quiet btn-sm" data-dl-show type="button">Show</button></div></div>` : ""}
    <h4 class="meta meta-saved">Saved links <span class="meta">(${(M.links || []).length})</span></h4>
    <div class="mod-list">${(M.links || []).map((l, i) => `<div class="mod-row"><div class="mod-info"><strong>${escapeHtml(l.name || l.url)}</strong><span class="meta" title="${escapeHtml(l.url)}">${escapeHtml(l.url)}</span>
      ${l.game ? `<span class="pill">${escapeHtml(l.game)}</span>` : ""}${l.notes ? `<span>${escapeHtml(l.notes)}</span>` : ""}</div>
      <div class="mod-act"><div class="actions"><button class="btn btn-secondary btn-sm" data-dl-reuse="${i}" type="button">Download</button><button class="btn btn-quiet btn-sm" data-dl-edit="${i}" type="button">Edit</button><button class="btn btn-quiet btn-sm" data-dl-del="${i}" type="button" aria-label="Delete">${SV(14,14,ICONS.x)}</button></div></div></div>`).join("") || `<div class="preview-empty">No saved links yet. Paste a URL and click Save link.</div>`}</div>
  </div>`;
}
async function runDownload(url, name) {
  if (!modsView) return;
  const id = uid();
  modsView.dl.job = { id, progress: 0, status: "Starting…", error: "", done: false };
  modsView.dl.last = null;
  render();
  setTaskbar(0);
  const off = N.onJobProgress(id, (p) => { const j = modsView?.dl?.job; if (!j || j.id !== id) return; j.progress = p.percent; j.status = p.file; paintJob(j); setTaskbar(p.percent); });
  try {
    const dest = modsView.dl.destDir || null;
    const maxBytes = Math.floor(N.settings.dlMaxGb * 1024 * 1024 * 1024);
    let expectedSha256 = (modsView.dl.sha || "").trim() || null;
    if (expectedSha256) {
      const hex = expectedSha256.toLowerCase();
      if (hex.length !== 64 || /[^0-9a-f]/.test(hex)) {
        throw new Error("Expected SHA-256 must be exactly 64 hexadecimal characters.");
      }
      expectedSha256 = hex;
    }
    const r = await N.invoke("download_url", { id, url, destDir: dest, expectedSha256, maxBytes });
    modsView.dl.job.progress = 100; modsView.dl.job.done = true; modsView.dl.job.status = `Saved ${r.name} (${formatBytes(r.bytes)})`;
    modsView.dl.last = r;
    if (N.ARCHIVE_RE.test(r.name) || /\.iso$/i.test(r.name)) {
      // extract via existing pipeline into downloads folder parent
      await addPaths([r.path]);
    } else {
      await addMods([r.path]);
      // DLC auto if STFS — queueDlc if looks like package
    }
    notifyDone("Unpack — download finished", `${r.name} · SHA-256 ${r.sha256.slice(0, 12)}…`);
  } catch (err) {
    if (modsView?.dl?.job) { modsView.dl.job.error = friendlyError(err); }
  } finally { off(); setTaskbar(null); render(); }
}

function bindMods() {
  const root = app.querySelector("[data-mods]");
  if (!root) return;
  const M = modsView;
  const close = () => { modsView = null; render(); };
  root.querySelector("[data-mods-close]")?.addEventListener("click", close);
  app.querySelector("[data-mods-overlay]")?.addEventListener("click", (e) => { if (e.target === e.currentTarget) close(); });
  root.querySelectorAll("[data-mods-tab]").forEach((b) => b.addEventListener("click", async () => { M.tab = b.dataset.modsTab; if (M.tab === "catalog") await openCatalog(); else render(); }));
  bindCatalog(root);
  const saveNames = () => { N.settings.modNames = M.names; M.names = N.settings.modNames; };
  root.querySelectorAll("[data-mn-del]").forEach((b) => b.addEventListener("click", () => { M.names = M.names.filter((_, i) => i !== +b.dataset.mnDel); M.namePick = null; saveNames(); render(); }));
  root.querySelectorAll("[data-mn-key]").forEach((inp) => inp.addEventListener("change", async () => {
    const i = +inp.dataset.mnKey, x = M.names[i];
    try {
      const k = await N.invoke("mod_name_key", { name: inp.value.slice(0, 200), tid: x.tid });
      if (M.names.some((y, j) => j !== i && y.key === k.key)) { toast("That name is already in the list."); }
      else { M.names = M.names.map((y, j) => (j === i ? { ...y, key: k.key } : y)); saveNames(); }
    } catch (e) { toast(friendlyError(e)); }
    render();
  }));
  root.querySelectorAll("[data-mn-remap]").forEach((b) => b.addEventListener("click", () => { M.namePick = { i: +b.dataset.mnRemap, q: "", results: null }; render(); root.ownerDocument.querySelector("[data-mn-gsearch]")?.focus(); }));
  root.querySelector("[data-mn-gsearch]")?.addEventListener("input", (e) => { clearTimeout(gsearchTimer); const q = e.target.value; gsearchTimer = setTimeout(async () => { M.namePick.q = q; M.namePick.results = q.trim() ? await N.invoke("game_search", { q }).catch(() => []) : null; render(); const el = app.querySelector("[data-mn-gsearch]"); if (el) { el.focus(); el.setSelectionRange(q.length, q.length); } }, 250); });
  root.querySelectorAll("[data-mn-gres]").forEach((b) => b.addEventListener("click", () => { const g = M.namePick.results[+b.dataset.mnGres], i = M.namePick.i; M.names = M.names.map((y, j) => (j === i ? { ...y, tid: g.title_id, gname: g.name } : y)); M.namePick = null; saveNames(); render(); }));
  root.querySelector("[data-mn-export]")?.addEventListener("click", () => exportModNames(M.names));
  root.querySelector("[data-mn-import]")?.addEventListener("click", () => {
    const input = document.createElement("input"); input.type = "file"; input.accept = ".json,.csv,text/csv,application/json";
    input.onchange = async () => { const f = input.files?.[0]; if (!f) return; if (f.size > 1024 * 1024) return toast("That list file is too large (max 1 MB)."); const t = await f.text(); await importModNamesText(t, /\.csv$/i.test(f.name) || /^\s*"?(key|name)"?\s*,/i.test(t)); };
    input.click();
  });
  root.querySelector("[data-dl-go]")?.addEventListener("click", () => {
    const url = root.querySelector("[data-dl-url]")?.value?.trim();
    if (!url) return toast("Paste a link first.");
    M.dl.url = url; M.dl.name = root.querySelector("[data-dl-name]")?.value || ""; M.dl.notes = root.querySelector("[data-dl-notes]")?.value || "";
    M.dl.game = root.querySelector("[data-dl-game]")?.value || ""; M.dl.sha = root.querySelector("[data-dl-sha-expect]")?.value || "";
    runDownload(url, M.dl.name);
  });
  root.querySelector("[data-dl-save]")?.addEventListener("click", () => {
    const url = root.querySelector("[data-dl-url]")?.value?.trim();
    if (!url) return toast("Paste a link first.");
    const name = root.querySelector("[data-dl-name]")?.value?.trim() || url;
    const notes = root.querySelector("[data-dl-notes]")?.value || "";
    const game = root.querySelector("[data-dl-game]")?.value?.trim() || "";
    const sha = root.querySelector("[data-dl-sha-expect]")?.value?.trim() || "";
    M.dl.url = url; M.dl.name = name; M.dl.notes = notes; M.dl.game = game; M.dl.sha = sha;
    M.links = [{ id: uid(), name, url, notes, game, sha }, ...M.links.filter((l) => l.url !== url)];
    N.settings.savedLinks = M.links;
    render();
  });
  root.querySelector("[data-dl-url]")?.addEventListener("input", (e) => { M.dl.url = e.target.value; /* re-render would steal focus — paint warn only */ const w = root.querySelector(".warn-text"); const http = /^http:\/\//i.test(e.target.value) && !/^https:\/\//i.test(e.target.value); if (w && !http) w.remove(); else if (!w && http) { const p = document.createElement("p"); p.className = "warn-text"; p.role = "status"; p.textContent = "This is plain HTTP (not HTTPS). Prefer an https:// link when you can."; e.target.parentElement.after(p); } });
  root.querySelector("[data-dl-usb-apps]")?.addEventListener("click", () => {
    if (!M.dl.last) return;
    const hb = matchHomebrewName(M.dl.last.name);
    copyDownloadToUsbApps(M.dl.last.path, (hb && hb.name) || M.dl.name || N.baseName(M.dl.last.name).replace(/\.[^.]+$/, ""));
  });
  root.querySelector("[data-dl-export]")?.addEventListener("click", () => exportSavedLinks(M.links || []));
  root.querySelector("[data-dl-import]")?.addEventListener("click", () => importSavedLinks(M));
  root.querySelector("[data-dl-folder]")?.addEventListener("click", async () => {
    const p = await N.pickFolder("Download into this folder");
    if (p) { M.dl.destDir = p; toast("Downloads go to " + p); }
  });
  root.querySelector("[data-dl-cancel]")?.addEventListener("click", () => { if (M.dl.job) N.invoke("cancel_job", { id: M.dl.job.id }); });
  root.querySelector("[data-dl-add-mod]")?.addEventListener("click", () => { if (M.dl.last) addMods([M.dl.last.path]); });
  root.querySelector("[data-dl-show]")?.addEventListener("click", () => { if (M.dl.last) window.__TAURI__.opener?.revealItemInDir(M.dl.last.path); });
  root.querySelectorAll("[data-dl-reuse]").forEach((b) => b.addEventListener("click", () => { const l = M.links[+b.dataset.dlReuse]; M.dl.url = l.url; M.dl.name = l.name; M.dl.notes = l.notes || ""; M.dl.game = l.game || ""; M.dl.sha = l.sha || ""; runDownload(l.url, l.name); }));
  root.querySelectorAll("[data-dl-edit]").forEach((b) => b.addEventListener("click", () => { const l = M.links[+b.dataset.dlEdit]; M.dl.url = l.url; M.dl.name = l.name; M.dl.notes = l.notes || ""; M.dl.game = l.game || ""; M.dl.sha = l.sha || ""; M.tab = "download"; render(); }));
  root.querySelectorAll("[data-dl-del]").forEach((b) => b.addEventListener("click", () => { M.links.splice(+b.dataset.dlDel, 1); N.settings.savedLinks = M.links; render(); }));

  root.querySelector("[data-mods-add-files]")?.addEventListener("click", async () => {
    const paths = await N.pickFiles("Add mod files you already have");
    if (paths.length) addMods(paths);
  });
  root.querySelector("[data-mods-add-folder]")?.addEventListener("click", async () => { const p = await N.pickFolder("Add a mod folder"); if (p) addMods([p]); });
  const q = root.querySelector("[data-mods-q]");
  q?.addEventListener("input", () => { M.q = q.value; const pos = q.selectionStart; render(); const n = app.querySelector("[data-mods-q]"); n?.focus(); n?.setSelectionRange(pos, pos); });
  root.querySelector("[data-mods-fgame]")?.addEventListener("change", (e) => { M.fGame = e.target.value; render(); });
  root.querySelector("[data-mods-ftype]")?.addEventListener("change", (e) => { M.fType = e.target.value; render(); });
  root.querySelectorAll("[data-mod-card]").forEach((b) => b.addEventListener("click", () => { M.sel = +b.dataset.modCard; M.pick = null; render(); }));
  const it = M.items[M.sel];
  root.querySelector("[data-mod-notes]")?.addEventListener("input", (e) => { it.notes = e.target.value; saveMods(); });
  root.querySelector("[data-mod-mode]")?.addEventListener("change", (e) => { it.mode = e.target.value; saveMods(); });
  root.querySelector("[data-mod-remove]")?.addEventListener("click", () => { M.items.splice(M.sel, 1); M.pick = null; saveMods(); render(); });
  root.querySelector("[data-mod-copy]")?.addEventListener("click", () => copyModToDrive(it));
  root.querySelector("[data-mod-xbox]")?.addEventListener("click", () => sendModToXbox(it));
  root.querySelector("[data-mod-show]")?.addEventListener("click", () => window.__TAURI__.opener?.revealItemInDir(it.path).catch((e) => toast(String(e))));
  root.querySelector("[data-mod-gchange]")?.addEventListener("click", () => { M.pick = { q: "", results: null }; render(); });
  root.querySelector("[data-mod-gcancel]")?.addEventListener("click", () => { M.pick = null; render(); });
  root.querySelectorAll("[data-mod-guess]").forEach((b) => b.addEventListener("click", () => { const g = it.info.guesses[+b.dataset.modGuess]; setModGame(it, g.title_id, g.name); }));
  root.querySelectorAll("[data-mod-gres]").forEach((b) => b.addEventListener("click", () => { const g = M.pick.results[+b.dataset.modGres]; setModGame(it, g.title_id, g.name); }));
  const gs = root.querySelector("[data-mod-gsearch]");
  gs?.addEventListener("input", () => {
    M.pick ||= { q: "", results: null };
    M.pick.q = gs.value;
    clearTimeout(gsearchTimer);
    gsearchTimer = setTimeout(async () => {
      M.pick.results = gs.value.trim() ? (await N.invoke("game_search", { q: gs.value }).catch(() => [])).slice(0, 12) : null;
      render();
      const n = app.querySelector("[data-mod-gsearch]"); n?.focus(); n?.setSelectionRange(n.value.length, n.value.length);
    }, 200);
  });
}

// ---- First-run tips ----
const TIPS_KEY = "unpack.tipsDone";
function renderTips() {
  if (!N.IS_DESKTOP || localStorage.getItem(TIPS_KEY) === "1") return "";
  return `<section class="tips" role="note"><div class="tips-art">${TIPS_ART}</div><div><strong>Welcome to Unpack</strong>
    <ul><li>Drop a pile of archives. Set <b>Extract to → A chosen folder</b> to put them all in one place.</li>
    <li>Set <b>Auto ISO</b> to GOD or Extracted folder so Xbox 360 ISOs inside archives become ready-to-copy. DLC and title updates go into their game.</li>
    <li><b>Library</b> shows your games with their DLC and can copy them to a USB drive. Press <kbd>?</kbd> for shortcuts.</li></ul></div>
    <button class="btn btn-secondary btn-sm" data-tips-done type="button">Got it</button></section>`;
}
const TIPS_ART = `<svg viewBox="0 0 48 48" width="44" height="44" aria-hidden="true"><circle cx="24" cy="20" r="12" fill="none" stroke="var(--accent)" stroke-width="2.5"/><path d="M19 36h10M20 40h8" stroke="var(--accent2)" stroke-width="2.5" stroke-linecap="round"/><path d="M24 12v4M18 18l2 2M30 18l-2 2" stroke="var(--warn)" stroke-width="2" stroke-linecap="round"/></svg>`;
let shortcutsOpen = false;
const SHORTCUTS = [["Ctrl + O", "Open archives"], ["Ctrl + ,", "Settings"], ["Ctrl + L", "Library"], ["Ctrl + M", "Mod Menus"], ["Ctrl + D", "Install DLC"], ["Ctrl + G", "ISO → GOD"], ["Ctrl + Shift + X", "Send to Xbox"], ["Ctrl + N", "Create archive"], ["?", "This list"], ["Esc", "Close a window"]];
function renderShortcuts() {
  if (!shortcutsOpen) return "";
  return `<div class="overlay" data-sc-overlay><div class="dialog" role="dialog" aria-modal="true" aria-labelledby="sc-h"><h3 id="sc-h">Keyboard shortcuts</h3>
    <div class="sc-list">${SHORTCUTS.map(([k, v]) => `<div><kbd>${escapeHtml(k)}</kbd><span>${escapeHtml(v)}</span></div>`).join("")}</div>
    <div class="actions actions-end-mt"><button class="btn btn-primary" data-sc-close type="button">Close</button></div></div></div>`;
}

// ---- Xbox 360 DLC: install STFS DLC packages into their game's <TitleID>/00000002 folder ----
let dlcManual = null; // pseudo job for manually picked DLC: { gods: [] }
const DLC_SHOW = new Set(["installed", "identical", "not_found", "title_update", "failed", "queued", "saving"]);
function dlcLibraries(extra = []) {
  const s = settings();
  return [...new Set([...extra, ...(s.destMode === "folder" && s.destPath ? [s.destPath] : []), ...N.settings.libraries])];
}
async function dlcKindForPath(p) {
  try {
    const [r] = await N.invoke("mod_identify", { paths: [p] });
    const info = r?.Ok;
    if (info?.kind === "stfs" && info.content_type === "000B0000") return "tu";
    if (info?.kind === "stfs" && info.content_type === "00000002") return "dlc";
  } catch {}
  return "dlc";
}
async function queueDlcFromExtract(holder, paths, extraLibs = []) {
  const kept = [];
  for (const p of paths) {
    const k = await dlcKindForPath(p);
    if (k === "tu" && !N.settings.autoTu) continue;
    if (k === "dlc" && !N.settings.autoDlc) continue;
    kept.push(p);
  }
  if (kept.length) queueDlc(holder, kept, extraLibs);
}
function queueDlc(holder, paths, extraLibs = []) {
  const rows = paths.map((p) => ({ id: uid(), dlc: true, path: p, name: N.baseName(p), phase: "queued", status: "DLC: waiting…", progress: 0, error: "", libs: dlcLibraries(extraLibs), gamePath: "" }));
  holder.dlcRows = [...(holder.dlcRows || []), ...rows];
  render();
  for (const d of rows) godChain = godChain.then(() => runDlc(d)); // after this batch's GOD conversions
}
let dlcPick = null; // { row, games: [{path,name}] }
async function runDlc(d) {
  if (d.phase !== "queued" && d.phase !== "retry") return;
  d.phase = "saving";
  d.status = "DLC: looking for the game…";
  render();
  try {
    const args = { path: d.path, libraries: d.libs, moveFile: N.settings.moveDlc };
    if (d.gamePath) args.gamePath = d.gamePath;
    const r = await N.invoke("dlc_install", args);
    if (r.status === "ambiguous") {
      const games = await N.invoke("dlc_match_games", { libraries: d.libs, titleId: r.title_id }).catch(() => []);
      d.phase = "queued";
      d.status = r.message || "Pick which game gets this package";
      d.titleId = r.title_id;
      await new Promise((resolve) => {
        dlcPick = { row: d, games: games.map((path) => ({ path, name: N.baseName(path) })), resolve };
        render();
      });
      if (!d.gamePath) { d.phase = "skipped"; d.status = "DLC skipped — game not chosen"; render(); return; }
      return runDlc(d);
    }
    d.result = r.status;
    d.progress = r.status === "installed" || r.status === "identical" ? 100 : 0;
    d.progress = r.status === "tu_installed" ? 100 : d.progress;
    d.phase = r.status === "installed" || r.status === "tu_installed" ? "saved" : r.status === "not_dlc" ? "hidden" : "skipped";
    d.status = (r.status === "installed" ? "DLC installed: " : r.status === "tu_installed" ? "Title update installed: " : "DLC: ") + r.message + (r.moved ? " · moved" : "");
    batchStats[r.status === "installed" || r.status === "tu_installed" ? "dlcOk" : r.status === "not_found" ? "dlcMissing" : "dlcOther"]++;
    if (r.name) d.name = `${r.name} (${N.baseName(d.path)})`;
    if (r.status === "installed" || r.status === "tu_installed") addHistory({ name: r.name || N.baseName(d.path), source: d.path, output: r.dest, result: "created", label: "DLC installed" });
  } catch (err) {
    batchStats.dlcFail++;
    d.phase = "failed";
    d.status = "DLC failed · file left where it is";
    d.error = friendlyError(err);
    showError(`${d.name}: ${d.error}`);
  }
  render();
}
/** A game may appear later in the batch (its ISO converts after the DLC ran): retry "not found". */
function retryDlcNotFound(newLibs = false) {
  for (const h of [...jobs, dlcManual].filter(Boolean)) for (const d of h.dlcRows || []) if (d.result === "not_found") { if (newLibs) d.libs = [...new Set([...d.libs, ...dlcLibraries()])]; d.result = ""; d.phase = "retry"; godChain = godChain.then(() => runDlc(d)); }
}
async function pickDlc() {
  const paths = await N.pickFiles("Pick Xbox 360 DLC packages");
  if (!paths.length) return;
  dlcManual ||= { dlcRows: [] };
  queueDlc(dlcManual, paths);
}
function renderDlcRows(rows) {
  return (rows || []).filter((d) => d.phase !== "hidden").map((d) => `<div class="qrow sub">
        <span class="qname" title="${escapeHtml(d.error || d.path)}"><span class="dot ${escapeHtml(d.phase === "retry" ? "queued" : d.phase)}"></span>${fileIcon("dlc")}<span class="name">${escapeHtml(d.name)}</span></span>
        <div class="qbar"><div data-job-bar="${escapeHtml(d.id)}" class="qbar-fill"></div></div>
        <span class="qstatus${d.phase === "failed" ? " danger" : ""}" title="${escapeHtml(d.error || d.status)}">${escapeHtml(d.error ? d.status + " — " + d.error : d.status)}</span><span></span>
      </div>`).join("");
}

function cancelJobGods(job) {
  for (const g of job.gods || []) {
    if (g.phase === "queued") { g.phase = "skipped"; g.status = "GOD cancelled · ISO kept"; }
    else if (g.phase === "saving") N.invoke("cancel_job", { id: g.id }).catch(() => {});
  }
  render();
}

// ---------------------------------------------------------------------------
// Batch queue: at most Settings > "Jobs at once" extractions at once, the rest wait their turn.
// ---------------------------------------------------------------------------
const waiting = [];
let running = 0;

async function enqueue(job, opts = {}) {
  if (job.phase === "saving" || job.phase === "queued") return;
  // Resolve "ask" destinations now, while the user is looking, not when the job's turn comes.
  if (!opts.dest) {
    const dest = await resolveDest(job, opts.mode);
    if (!dest) return;
    opts = { ...opts, dest };
  }
  if (job.phase === "saving" || job.phase === "queued") return; // double click while the dialog was open
  job.phase = "queued";
  job.status = "Waiting in queue…";
  job.progress = 0;
  job.cancelled = false;
  waiting.push({ job, opts });
  render();
  pump();
}

function waitDestKey(w) {
  const layout = w.opts?.layout || "folder";
  return `${w.opts?.dest || ""}|${layout}|${layout === "here" ? w.job.path : ""}`;
}
function moveWaiting(jobId, where) {
  const i = waiting.findIndex((w) => w.job.id === jobId);
  if (i < 0) return;
  const key = waitDestKey(waiting[i]);
  if (where === "front") {
    let t = 0;
    while (t < waiting.length && waitDestKey(waiting[t]) === key) t++;
    const [w] = waiting.splice(i, 1);
    waiting.splice(t, 0, w);
  } else if (where === "up" && i > 0) {
    const j = i - 1;
    if (waitDestKey(waiting[j]) !== key) {
      const [w] = waiting.splice(i, 1);
      waiting.splice(j, 0, w);
    } else [waiting[i - 1], waiting[i]] = [waiting[i], waiting[i - 1]];
  } else if (where === "down" && i < waiting.length - 1) {
    const j = i + 1;
    if (waitDestKey(waiting[j]) !== key) {
      const [w] = waiting.splice(i, 1);
      waiting.splice(j, 0, w);
    } else [waiting[i + 1], waiting[i]] = [waiting[i], waiting[i + 1]];
  }
  render();
}
function pump() {
  if (N.settings.queuePaused) return;
  while (running < N.settings.maxJobs && waiting.length) {
    const { job, opts } = waiting.shift();
    if (job.cancelled) continue;
    running += 1;
    job.phase = "ready"; // extractNative flips it to "saving"
    extractNative(job, opts).finally(() => {
      running -= 1;
      pump();
    });
  }
}

function cancelJob(job) {
  const i = waiting.findIndex((w) => w.job === job);
  if (i >= 0) {
    waiting.splice(i, 1);
    job.cancelled = true;
    job.phase = "ready";
    job.status = "Removed from queue";
    render();
    return;
  }
  if (job.phase === "saving") cancelNative(job);
}

// ---------------------------------------------------------------------------
// Recent jobs history (kept in localStorage, newest first)
// ---------------------------------------------------------------------------
const HISTORY_KEY = "unpack.history";
const HISTORY_MAX = 40;
function loadHistory() {
  try {
    const h = JSON.parse(localStorage.getItem(HISTORY_KEY) || "[]");
    return Array.isArray(h) ? h : [];
  } catch {
    return [];
  }
}
function addHistory(entry) {
  const h = [{ ...entry, at: Date.now() }, ...loadHistory()].slice(0, HISTORY_MAX);
  try { localStorage.setItem(HISTORY_KEY, JSON.stringify(h)); } catch {}
}
function timeAgo(t) {
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return Math.floor(s / 60) + " min ago";
  if (s < 86400) return Math.floor(s / 3600) + " h ago";
  return new Date(t).toLocaleDateString();
}
function renderHistory() {
  const h = loadHistory();
  if (!N.IS_DESKTOP || !h.length) return "";
  const rows = h
    .slice(0, 12)
    .map((e, i) => {
      const mark = e.result === "saved" || e.result === "created" ? "saved" : e.result === "skipped" ? "skipped" : "failed";
      return `<div class="hist-row"><span class="dot ${mark}"></span>
        <span class="name" title="${escapeHtml(e.output || e.source || "")}">${escapeHtml(e.name)}</span>
        <span class="size">${escapeHtml(e.label || e.result)}</span>
        <span class="size">${escapeHtml(timeAgo(e.at))}</span>
        ${e.output && mark !== "failed" ? `<button class="btn btn-quiet" data-reveal="${escapeHtml(String(e.at))}" type="button">Show</button>` : `<span></span>`}
      </div>`;
    })
    .join("");
  return `<section class="panel hist"><div class="panel-top"><strong class="hist-title">Recent</strong><button class="btn btn-quiet" data-action="clear-history" type="button">Clear</button></div>${rows}</section>`;
}

onJobDone = (job, layout, opts = {}) => {
  const failed = job.phase === "failed" && job.status !== "Cancelled";
  batchStats[job.phase === "saved" ? "extracted" : job.phase === "skipped" ? "skipped" : job.status === "Cancelled" ? "skipped" : "failed"]++;
  if (failed) batchStats.failedJobs.push({ id: job.id, opts });
  if (job.phase === "skipped" && job.status !== "Cancelled") batchStats.skippedJobs.push({ id: job.id, opts });
  if (job.output && job.phase === "saved") batchStats.lastOutput = job.output;
  scheduleSummary();
  addHistory({
    name: job.name,
    source: job.path,
    output: job.output || "",
    result: job.phase === "saved" ? "saved" : job.phase === "skipped" ? "skipped" : job.status === "Cancelled" ? "cancelled" : "failed",
    label: job.phase === "saved" ? (layout === "here" ? "Extracted here" : "Extracted") : job.phase === "skipped" ? "Skipped" : job.status === "Cancelled" ? "Cancelled" : "Failed",
  });
};

// ---------------------------------------------------------------------------
// Create archives (ZIP / 7Z) from dropped or picked files and folders
// ---------------------------------------------------------------------------
let composer = null;

function onNonArchives(items, archiveCount) {
  const isos = items.filter((i) => !i.is_dir && /\.iso$/i.test(i.name));
  if (isos.length && isos.length === items.length) return openGod(isos);
  if (!nativeOk) {
    if (!archiveCount) toast("Drop a .zip, .rar, or .7z file.");
    return;
  }
  openComposer(items);
}

function openComposer(items = []) {
  if (composer?.busy) {
    toast("An archive is still being created.");
    return;
  }
  if (!composer) {
    composer = { id: uid(), items: [], format: "zip", password: "", level: 5, busy: false, progress: 0, status: "", error: "", name: "" };
  }
  for (const it of items) if (!composer.items.some((x) => x.path === it.path)) composer.items.push(it);
  if (!composer.name && composer.items.length) {
    composer.name = composer.items.length === 1 ? composer.items[0].name.replace(/\.[^.]+$/, "") || composer.items[0].name : N.baseName(N.dirName(composer.items[0].path)) || "archive";
  }
  render();
}

function renderComposer() {
  if (!composer) return "";
  const c = composer;
  const total = c.items.reduce((n, i) => n + (i.is_dir ? 0 : i.size), 0);
  const rows = c.items
    .map((it, i) => `<div class="row"><span>${icon(it.is_dir ? "folder" : "file")}</span><span class="name" title="${escapeHtml(it.path)}">${escapeHtml(it.name)}</span><span class="size">${it.is_dir ? "folder" : formatBytes(it.size)}</span>${c.busy ? "" : `<button class="btn btn-quiet" data-cmp-remove="${i}" type="button" aria-label="Remove">×</button>`}</div>`)
    .join("");
  return `<div class="overlay"><form class="dialog dialog-wide" data-composer>
    <h3>Create archive</h3>
    <p>${c.items.length} item${c.items.length === 1 ? "" : "s"}${total ? " · " + formatBytes(total) + " of files" : ""}. Drop more files or folders onto the window to add them.</p>
    <div class="cmp-list">${rows || `<div class="preview-empty">Nothing added yet.</div>`}</div>
    <div class="actions actions-my1">
      <button class="btn btn-secondary" data-cmp-add-files type="button" ${c.busy ? "disabled" : ""}>Add files</button>
      <button class="btn btn-secondary" data-cmp-add-folder type="button" ${c.busy ? "disabled" : ""}>Add folder</button>
    </div>
    <div class="cmp-grid">
      <label>Name<input data-cmp-name value="${escapeHtml(c.name)}" ${c.busy ? "disabled" : ""} /></label>
      <label>Format<select data-cmp-format ${c.busy ? "disabled" : ""}>
        <option value="zip" ${c.format === "zip" ? "selected" : ""}>ZIP — opens anywhere</option>
        <option value="7z" ${c.format === "7z" ? "selected" : ""}>7Z — smaller</option>
      </select></label>
      <label>Compression<select data-cmp-level ${c.busy ? "disabled" : ""}>
        <option value="1" ${c.level == 1 ? "selected" : ""}>Fast</option>
        <option value="5" ${c.level == 5 ? "selected" : ""}>Normal</option>
        <option value="9" ${c.level == 9 ? "selected" : ""}>Maximum</option>
        <option value="0" ${c.level == 0 ? "selected" : ""}>Store only</option>
      </select></label>
      <label>Password (optional)<input data-cmp-pw type="password" autocomplete="new-password" value="${escapeHtml(c.password)}" ${c.busy ? "disabled" : ""} /></label>
    </div>
    <p class="meta meta-mb8">${c.password ? (c.format === "7z" ? "AES-256. File names are hidden too." : "AES-256. File names stay visible in ZIPs; use 7Z to hide them. Older Windows Explorer can't open AES ZIPs — 7-Zip or Unpack can.") : "No password: anyone can open it."}</p>
    ${c.busy || c.progress ? `<div class="progress"><div data-job-bar="${escapeHtml(c.id)}" class="qbar-fill"></div></div>` : ""}
    <div class="status-line"><span data-job-status="${escapeHtml(c.id)}">${escapeHtml(c.status)}</span></div>
    ${c.error ? `<div class="error-box danger">${escapeHtml(c.error)}</div>` : ""}
    <div class="actions actions-mt1">
      ${c.busy ? `<button class="btn btn-secondary" data-cmp-cancel type="button">Cancel</button>` : `<button class="btn btn-primary" type="submit" ${c.items.length ? "" : "disabled"}>Create…</button><button class="btn btn-secondary" data-cmp-close type="button">Close</button>`}
    </div>
  </form></div>`;
}

function bindComposer() {
  const form = app.querySelector("[data-composer]");
  if (!form || !composer) return;
  const c = composer;
  form.querySelector("[data-cmp-name]")?.addEventListener("input", (e) => { c.name = e.target.value; });
  form.querySelector("[data-cmp-pw]")?.addEventListener("input", (e) => {
    c.password = e.target.value;
    const note = form.querySelector(".meta");
    if (note) note.textContent = c.password ? (c.format === "7z" ? "AES-256. File names are hidden too." : "AES-256. File names stay visible in ZIPs; use 7Z to hide them. Older Windows Explorer can't open AES ZIPs — 7-Zip or Unpack can.") : "No password: anyone can open it.";
  });
  form.querySelector("[data-cmp-format]")?.addEventListener("change", (e) => { c.format = e.target.value; render(); });
  form.querySelector("[data-cmp-level]")?.addEventListener("change", (e) => { c.level = Number(e.target.value); });
  form.querySelectorAll("[data-cmp-remove]").forEach((b) => b.addEventListener("click", () => { c.items.splice(Number(b.dataset.cmpRemove), 1); render(); }));
  form.querySelector("[data-cmp-close]")?.addEventListener("click", () => { composer = null; render(); });
  form.querySelector("[data-cmp-cancel]")?.addEventListener("click", () => N.invoke("cancel_job", { id: c.id }).catch(() => {}));
  const add = async (directory) => {
    const res = await window.__TAURI__.dialog.open({ multiple: true, directory, title: directory ? "Add folders" : "Add files" });
    const paths = res ? (Array.isArray(res) ? res : [res]) : [];
    const infos = [];
    for (const p of paths) infos.push(await N.invoke("file_info", { path: p }));
    openComposer(infos);
  };
  form.querySelector("[data-cmp-add-files]")?.addEventListener("click", () => add(false).catch((e) => toast(String(e))));
  form.querySelector("[data-cmp-add-folder]")?.addEventListener("click", () => add(true).catch((e) => toast(String(e))));
  form.addEventListener("submit", (e) => { e.preventDefault(); createArchive(); });
}

async function createArchive() {
  const c = composer;
  if (!c || c.busy || !c.items.length) return;
  const ext = "." + c.format;
  const base = safeSegment((c.name || "archive").trim()).replace(/\.(zip|7z)$/i, "") || "archive";
  const dir = N.dirName(c.items[0].path);
  const sep = dir.includes("\\") ? "\\" : "/";
  let out = await N.pickSave(dir.endsWith(sep) ? dir + base + ext : dir + sep + base + ext, [{ name: c.format.toUpperCase() + " archive", extensions: [c.format] }]);
  if (!out) { toast("Save cancelled."); return; }
  if (!out.toLowerCase().endsWith(ext)) out += ext;
  c.busy = true;
  c.progress = 0;
  c.error = "";
  c.status = "Starting…";
  render();
  const off = N.onJobProgress(c.id, (p) => {
    c.progress = p.percent;
    c.status = `Adding… ${p.percent}%` + (p.file ? " · " + p.file : "");
    schedulePaint();
    paintJob(c);
  });
  try {
    const made = await N.invoke("create_archive", { id: c.id, sources: c.items.map((i) => i.path), outFile: out, format: c.format, password: c.password || null, level: c.level });
    c.status = "Created " + made;
    c.progress = 100;
    toast("Created " + N.baseName(made));
    onArchiveCreated?.(c, made);
    c.items = [];
    c.name = "";
    c.password = "";
  } catch (err) {
    const code = N.errCode(err);
    c.status = code === "CANCELLED" ? "Cancelled" : "Failed";
    c.error = code === "CANCELLED" ? "" : friendlyError(err);
    if (c.error) showError(c.error);
    c.progress = 0;
    onArchiveCreated?.(c, null, c.status);
  } finally {
    off();
    c.busy = false;
    c.id = uid(); // fresh id for the next run so stale progress events can't leak in
    render();
  }
}
// ---------------------------------------------------------------------------
// Xbox 360 ISO -> Games on Demand (iso2god-rs, MIT). One conversion at a time.
// ---------------------------------------------------------------------------
let godPanel = null; // { items: [...], busy }
const GOD_DEST_KEY = "unpack.godDest";

async function openGod(infos = []) {
  if (!godPanel) godPanel = { items: [], busy: false };
  for (const info of infos) {
    if (godPanel.items.some((x) => x.path === info.path)) continue;
    const item = { id: uid(), path: info.path, name: info.name, title: "", titleId: "", kind: "", parts: 0, size: info.size, phase: "reading", status: "Reading ISO…", progress: 0, error: "" };
    godPanel.items.push(item);
    N.invoke("iso_info", { path: info.path })
      .then((r) => {
        item.titleId = r.title_id;
        item.kind = r.kind;
        item.parts = r.parts;
        item.disc = r.disc_number || 1;
        item.discs = r.disc_count || 1;
        item.warning = r.known === false ? unknownTitleText(r.title_id) : "";
        if (!item.title) item.title = r.name || item.name.replace(/\.iso$/i, "");
        item.phase = "ready";
        item.status = `${r.kind} · Title ID ${r.title_id}${item.discs > 1 || item.disc > 1 ? ` · Disc ${item.disc}${item.discs > 1 ? ` of ${item.discs}` : ""}` : ""} · ${formatBytes(r.data_size)} · ${r.parts} part${r.parts === 1 ? "" : "s"}`;
        offerCompanions(item).then(() => { for (const o of godPanel?.items || []) if (o !== item && o.phase === "ready" && o.discs > 1) offerCompanions(o); });
      })
      .catch((err) => {
        item.phase = "failed";
        item.status = "Can't convert";
        item.error = friendlyError(err);
      })
      .finally(() => godPanel && render());
  }
  render();
}

const unknownTitleText = (tid) => `Title ID ${tid} is not in Unpack's game list. It is converted anyway; the folder is named after the file (or the title you type), or, in a Content folder, after its TitleID.`;

/** " · Disc 2 of 2, added to the folder of disc 1" for a finished convert. */
function discNote(r) {
  if (!r) return "";
  const multi = (r.disc_count || 1) > 1 || (r.disc_number || 1) > 1;
  if (!multi && !r.merged) return "";
  return ` · Disc ${r.disc_number || 1}${r.disc_count > 1 ? ` of ${r.disc_count}` : ""}${r.merged ? ", merged into the same game folder" : ""}`;
}

/** Other discs of the same game next to a picked ISO: offer them, never add them silently.
 *  Also fills the per-game "Disc 1 of 2: …" line and the missing-disc warning. */
async function offerCompanions(item) {
  const g = godPanel;
  if (!g) return;
  const st = await discStatus(item.path, g.items.filter((x) => x !== item && x.phase !== "failed" && x.phase !== "done").map((x) => x.path), localStorage.getItem(GOD_DEST_KEY));
  if (godPanel !== g) return;
  if (st) {
    item.disc = st.disc || item.disc || 1;
    item.discs = Math.max(item.discs || 1, st.count || 1);
    item.discLine = st.line;
    item.discWarn = st.missing.length ? missingText(st) : "";
  }
  render();
  const mode = N.settings.companionDiscMode;
  if (mode === "never" || !st) return;
  const fresh = st.slots.filter((x) => x.state === "next_to_it" && x.path).map((x) => x.path).filter((p) => !g.items.some((x) => x.path === p) && !(g.declined || []).includes(p));
  if (!fresh.length) return;
  // Disc 2+ picked alone with no disc 1 anywhere: ask instead of adding silently.
  const soloDisc2 = item.disc > 1 && !st.slots.some((x) => x.disc === 1 && x.state !== "missing");
  if (mode === "always" && !soloDisc2) {
    const infos = [];
    for (const p of fresh) infos.push(await N.invoke("file_info", { path: p }));
    openGod(infos);
    return;
  }
  g.offer = [...new Set([...(g.offer || []), ...fresh])].filter((p) => !g.items.some((x) => x.path === p));
  render();
}
/** Re-run the disc check for every ready ISO (after one is added or removed). */
function refreshDiscs() {
  if (!godPanel) return;
  for (const i of godPanel.items) if (i.phase === "ready" && i.discLine !== undefined) offerCompanions(i);
}

function renderGod() {
  if (!godPanel) return "";
  const g = godPanel;
  const dest = localStorage.getItem(GOD_DEST_KEY) || "";
  const ready = g.items.filter((i) => i.phase === "ready").length;
  const rows = g.items
    .map(
      (i) => `<div class="god-row">
      <div class="god-top"><span class="dot ${i.phase === "done" ? "saved" : i.phase === "failed" ? "failed" : i.phase === "working" || i.phase === "queued" ? "saving" : "ready"}"></span>
        ${fileIcon(i.phase === "done" ? "god" : i.name)}<span class="name" title="${escapeHtml(i.path)}">${escapeHtml(i.name)}</span>${i.auto ? `<span class="pill">AUTO</span>` : ""}
        ${i.phase === "working" || i.phase === "queued" ? `<button class="btn btn-quiet" data-god-cancel="${escapeHtml(i.id)}" type="button">Cancel</button>` : g.busy && i.phase !== "done" ? "" : `<button class="btn btn-quiet" data-god-remove="${escapeHtml(i.id)}" type="button" aria-label="Remove">×</button>`}</div>
      <label class="god-title">Game title <input data-god-title="${escapeHtml(i.id)}" value="${escapeHtml(i.title)}" ${i.phase === "ready" || i.phase === "failed" ? "" : "disabled"} placeholder="Shown on the Xbox dashboard" maxlength="63" /></label>
      <div class="qbar"><div data-job-bar="${escapeHtml(i.id)}" class="qbar-fill"></div></div>
      <div class="qstatus" data-job-status="${escapeHtml(i.id)}">${escapeHtml(i.status)}</div>
      ${i.discLine ? `<div class="meta" data-god-discs>${escapeHtml(i.discLine)}</div>` : ""}
      ${i.discWarn && i.phase !== "done" ? `<div class="warn-text" data-god-missing>⚠ ${escapeHtml(i.discWarn)}</div>` : ""}
      ${i.warning && i.phase !== "failed" ? `<div class="error-box" data-god-warning>${escapeHtml(i.warning)}</div>` : ""}
      ${i.merged ? `<div class="meta">This disc was added to the same game folder as the other disc, so the console shows one game.</div>` : ""}
      ${i.error ? `<div class="error-box danger">${escapeHtml(i.error)}</div>` : ""}
    </div>`
    )
    .join("");
  const offer = g.offer?.length && !g.busy ? `<div class="error-box" data-god-offer>Other discs of this game were found next to it: ${g.offer.map((p) => `<code>${escapeHtml(N.baseName(p))}</code>`).join(", ")}. Convert them together so the console shows one game?
      <div class="actions actions-mt5"><button class="btn btn-secondary btn-sm" data-god-offer-yes type="button">Add ${g.offer.length === 1 ? "it" : "them"}</button><button class="btn btn-quiet btn-sm" data-god-offer-no type="button">No thanks</button></div></div>` : "";
  const verify = g.verifyPath ? `<div class="meta" data-god-verify-out>${escapeHtml(g.verifyMsg || "")}</div>` : "";
  return `<div class="overlay"><div class="dialog dialog-wide" data-god>
    <h3>Xbox 360 ISO → GOD / extracted folder</h3>
    <p>Turns Xbox 360 (and original Xbox) disc images into either Games on Demand (GOD) for HDD launch, or an extracted folder with <code>default.xex</code> and all game files (needed by many mods). Copy GOD Title-ID folders to <code>Content/0000000000000000/</code> (pick that folder, the <code>Content</code> folder or the drive itself as output and the game goes straight into <code>&lt;TitleID&gt;\\00007000</code>, with no "(GOD)" folder around it; another disc joins the same TitleID folder, and a disc that's already there is skipped), or extracted games to <code>Games\&lt;Name&gt;</code>. Only convert games you own.</p>
    <div class="cmp-list">${rows || `<div class="preview-empty">Add ISO files, or drop them onto the window.</div>`}</div>
    ${offer}
    <div class="actions actions-my08">
      <button class="btn btn-secondary" data-god-add type="button">Add ISOs</button>
      <button class="btn btn-secondary" data-god-dest type="button" ${g.busy ? "disabled" : ""}>${dest ? "Change output folder" : "Choose output folder"}</button>
      <button class="btn btn-quiet" data-god-verify type="button" ${g.busy ? "disabled" : ""} title="Check an existing GOD folder without writing">Verify only…</button>
    </div>
    ${verify}
    <p class="meta meta-mb8">Output: ${dest ? escapeHtml(dest) : "not chosen yet"}${g.items.some((i) => i.auto) ? " (auto conversions go next to each ISO)" : ""}. Needs about as much free space as each game.</p>
    <div class="actions">
      ${g.busy ? `<button class="btn btn-secondary" data-god-cancel-all type="button">Cancel all</button>` : `<button class="btn btn-primary" data-god-go type="button" ${ready && dest ? "" : "disabled"} title="Games on Demand">Convert to GOD ${ready || ""}</button><button class="btn btn-secondary" data-god-xex type="button" ${ready && dest ? "" : "disabled"} title="Extract XDVDFS to Game Name\\ with default.xex">Extract folder (XEX) ${ready || ""}</button><button class="btn btn-quiet" data-god-close type="button">Close</button>`}
    </div>
  </div></div>`;
}

function bindGod() {
  const root = app.querySelector("[data-god]");
  if (!root || !godPanel) return;
  const g = godPanel;
  const find = (id) => g.items.find((i) => i.id === id);
  root.querySelectorAll("[data-god-title]").forEach((el) => el.addEventListener("input", () => { const i = find(el.dataset.godTitle); if (i) i.title = el.value; }));
  root.querySelectorAll("[data-god-remove]").forEach((el) => el.addEventListener("click", () => { g.items = g.items.filter((i) => i.id !== el.dataset.godRemove); render(); refreshDiscs(); }));
  root.querySelectorAll("[data-god-cancel]").forEach((el) => el.addEventListener("click", () => cancelGod(find(el.dataset.godCancel))));
  root.querySelector("[data-god-cancel-all]")?.addEventListener("click", () => g.items.forEach((i) => cancelGod(i)));
  root.querySelector("[data-god-close]")?.addEventListener("click", () => { godPanel = null; render(); });
  root.querySelector("[data-god-dest]")?.addEventListener("click", async () => {
    const p = await N.pickFolder("Where should the GOD folders go?");
    if (p) { localStorage.setItem(GOD_DEST_KEY, p); render(); refreshDiscs(); }
  });
  root.querySelector("[data-god-add]")?.addEventListener("click", async () => {
    const res = await window.__TAURI__.dialog.open({ multiple: true, title: "Pick Xbox ISOs", filters: [{ name: "Disc images", extensions: ["iso"] }] });
    const paths = res ? (Array.isArray(res) ? res : [res]) : [];
    const infos = [];
    for (const p of paths) infos.push(await N.invoke("file_info", { path: p }));
    openGod(infos);
  });
  root.querySelector("[data-god-offer-yes]")?.addEventListener("click", async () => {
    const paths = g.offer || [];
    g.offer = [];
    const infos = [];
    for (const p of paths) infos.push(await N.invoke("file_info", { path: p }));
    openGod(infos);
  });
  root.querySelector("[data-god-offer-no]")?.addEventListener("click", () => { g.declined = [...(g.declined || []), ...(g.offer || [])]; g.offer = []; render(); });
  root.querySelector("[data-god-go]")?.addEventListener("click", () => runGod(true, "god"));
  root.querySelector("[data-god-xex]")?.addEventListener("click", () => runGod(true, "xex"));
  root.querySelector("[data-god-verify]")?.addEventListener("click", async () => {
    const p = await N.pickFolder("Pick a GOD Title-ID folder to verify (read-only)");
    if (!p) return;
    g.verifyPath = p;
    g.verifyMsg = "Checking…";
    render();
    try {
      const msg = await N.invoke("god_verify_only", { path: p });
      g.verifyMsg = msg || "Looks OK.";
    } catch (err) {
      g.verifyMsg = String(err?.message || err);
      showError(g.verifyMsg);
    }
    render();
  });
}

function cancelGod(item) {
  if (!item) return;
  if (item.phase === "queued") {
    item.phase = "ready";
    item.status = "Removed from queue";
    item.progress = 0;
    render();
  } else if (item.phase === "working") {
    N.invoke("cancel_job", { id: item.id }).catch(() => {});
  }
}

async function runGod(manual = true, mode = "god") {
  const g = godPanel;
  const dest = localStorage.getItem(GOD_DEST_KEY);
  if (!g || g.busy) return;
  if (manual) {
    if (!dest) return;
    // Discs of one game go lowest disc first (the lowest disc keeps the playable default.xex).
    g.items.sort((a, b) => (a.titleId || "").localeCompare(b.titleId || "") || (a.disc || 1) - (b.disc || 1));
    g.items.filter((i) => i.phase === "ready").forEach((i) => { i.phase = "queued"; i.auto = false; i.status = "Waiting…"; i.progress = 0; i.error = ""; i.outMode = mode; });
  }
  g.busy = true;
  render();
  let item;
  while ((item = godPanel === g && g.items.find((i) => i.phase === "queued"))) {
    const outMode = item.outMode || mode || "god";
    item.phase = "working";
    item.status = "Starting…";
    render();
    const off = N.onJobProgress(item.id, (p) => {
      item.progress = p.percent;
      item.status = `${p.percent}% · ${p.file}`;
      schedulePaint();
      paintJob(item);
    });
    try {
      const cmd = outMode === "xex" ? "iso_to_xex" : "iso_to_god";
      const r = await N.invoke(cmd, { id: item.id, path: item.path, dest: item.auto ? null : dest, title: item.title || null });
      item.phase = "done";
      item.progress = 100;
      const detail = outMode === "xex" ? `${r.files} files` : `${r.parts} part${r.parts === 1 ? "" : "s"}`;
      item.status = `Done · ${detail} → ${r.output}${discNote(r)}`;
      item.merged = !!r.merged;
      if (r.warning) item.warning = r.warning;
      const lab = outMode === "xex" ? (item.auto ? "XEX auto-extracted" : "XEX extracted") : (item.auto ? "GOD auto-converted" : "GOD converted");
      addHistory({ name: item.title || item.name, source: item.path, output: r.output, result: "created", label: lab });
      if (item.auto) toast(`${item.title || item.name} → ${outMode === "xex" ? "extracted folder" : "GOD"}.`);
    } catch (err) {
      const code = N.errCode(err);
      const raw = String(err?.message || err || "");
      if (/^Skipped: this disc/.test(raw)) {
        // Content folder: this MediaID is already there, nothing was changed.
        item.phase = "done";
        item.progress = 0;
        item.status = raw;
        addHistory({ name: item.title || item.name, source: item.path, output: "", result: "skipped", label: "ISO skipped (already there)" });
        continue;
      }
      item.phase = code === "CANCELLED" ? "ready" : "failed";
      item.status = code === "CANCELLED" ? "Cancelled" : "Failed";
      item.error = code === "CANCELLED" ? "" : friendlyError(err);
      if (item.error) showError(`${item.name}: ${item.error}`);
      item.progress = 0;
      addHistory({ name: item.title || item.name, source: item.path, output: "", result: code === "CANCELLED" ? "cancelled" : "failed", label: code === "CANCELLED" ? "ISO cancelled" : "ISO convert failed" });
    } finally {
      off();
      item.id = uid();
      render();
    }
  }
  g.busy = false;
  render();
}

let onArchiveCreated = (c, made, status) =>
  addHistory({ name: made ? N.baseName(made) : c.name || "archive", output: made || "", result: made ? "created" : status === "Cancelled" ? "cancelled" : "failed", label: made ? "Created" : status || "Failed" });

function cancelNative(job) {
  job.cancelled = true;
  N.invoke("cancel_job", { id: job.id }).catch(() => {});
}

async function previewNative(file) {
  const job = activeJob();
  if (!job || file.data || file.loading || file.previewError) return;
  if (!(isText(file.path) || isImage(file.path)) || file.size > 4 * 1024 * 1024) return;
  file.loading = true;
  try {
    const buf = await N.invoke("read_entry", { path: job.path, entry: file.path, password: job.password || null });
    file.data = new Uint8Array(buf);
  } catch (err) {
    file.previewError = friendlyError(err);
  } finally {
    file.loading = false;
  }
  if (selectedPath === file.path && activeJob() === job) {
    const pane = document.getElementById("preview");
    if (pane) {
      pane.innerHTML = renderPreview(file);
      pane.querySelector("[data-action=save-one]")?.addEventListener("click", () => downloadFile(file));
    }
  }
}

async function saveEntryNative(file) {
  const job = activeJob();
  if (!job) return;
  const name = file.path.split("/").pop() || "file";
  const dir = N.dirName(job.path);
  const sep = dir.includes("\\") ? "\\" : "/";
  const out = await N.pickSave(dir.endsWith(sep) ? dir + name : dir + sep + name);
  if (!out) return;
  const id = uid();
  toast("Saving " + name + "…");
  try {
    await N.invoke("extract_entry", { id, path: job.path, entry: file.path, password: job.password || null, outFile: out });
    toast("Saved " + N.baseName(out));
  } catch (err) {
    toast(friendlyError(err));
  }
}

function makeSample() {
  const files = {
    "Welcome.txt": strToU8(
      "Unpack sample archive\n\nDrop a .zip, .rar, or .7z onto the page.\nChoose a folder and the files land there with folders intact.\nNothing is uploaded — extraction stays in this tab.\n"
    ),
    "Projects/notes.md": strToU8("# Notes\n- ZIP uses a fast JS decoder\n- RAR and 7Z use WebAssembly in the browser\n"),
    "Photos/mark.svg": strToU8(
      '<svg xmlns="http://www.w3.org/2000/svg" width="240" height="140"><rect width="240" height="140" rx="16" fill="#221f1b"/><text x="28" y="80" fill="#c4b8a4" font-size="28" font-family="Georgia">Unpack</text></svg>'
    ),
    "data/sample.json": strToU8(JSON.stringify({ ok: true, formats: ["zip", "rar", "7z"] }, null, 2)),
  };
  const zipped = zipSync(files);
  return new File([zipped], "Unpack-sample.zip", { type: "application/zip" });
}

function buildTree(files) {
  const root = [];
  const folders = new Map();
  const ensure = (parts) => {
    let list = root;
    let key = "";
    for (const part of parts) {
      key = key ? key + "/" + part : part;
      if (!folders.has(key)) {
        const node = { name: part, path: key, type: "dir", children: [] };
        folders.set(key, node);
        list.push(node);
      }
      list = folders.get(key).children;
    }
    return list;
  };
  for (const file of files) {
    const parts = file.path.split("/");
    const name = parts.pop();
    const list = parts.length ? ensure(parts) : root;
    list.push({ name, path: file.path, type: "file", size: file.size, file });
  }
  const sort = (nodes) => {
    nodes.sort((a, b) => (a.type === b.type ? a.name.localeCompare(b.name) : a.type === "dir" ? -1 : 1));
    nodes.filter((n) => n.children).forEach((n) => sort(n.children));
  };
  sort(root);
  return root;
}

function filterTree(nodes, q) {
  if (!q.trim()) return nodes;
  const needle = q.trim().toLowerCase();
  const walk = (list) =>
    list
      .map((n) => {
        if (n.type === "file") return n.name.toLowerCase().includes(needle) || n.path.toLowerCase().includes(needle) ? n : null;
        const children = walk(n.children || []);
        if (children.length || n.name.toLowerCase().includes(needle)) return { ...n, children };
        return null;
      })
      .filter(Boolean);
  return walk(nodes);
}

// ---- Original icon set (hand-drawn SVG, no third-party packs) ----
const SV = (w, h, body, extra = "") => `<svg width="${w}" height="${h}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" ${extra}>${body}</svg>`;
const ICONS = {
  // Send to Xbox: a controller-shaped disc with a signal arc
  xbox: '<ellipse cx="12" cy="13" rx="8.5" ry="6.5"/><circle cx="12" cy="13" r="2.2"/><path d="M8 5.5c2.4-1.6 5.6-1.6 8 0"/>',
  folder: '<path d="M3 7.5a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  file: '<path d="M7 3h7l5 5v11a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"/><path d="M14 3v5h5"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 2.5v2.2M12 19.3v2.2M4.6 4.6l1.6 1.6M17.8 17.8l1.6 1.6M2.5 12h2.2M19.3 12h2.2M4.6 19.4l1.6-1.6M17.8 6.2l1.6-1.6"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  pack: '<path d="M4 8l8-4 8 4-8 4z"/><path d="M4 8v8l8 4 8-4V8"/><path d="M12 12v8"/>',
  disc: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2.5"/><path d="M16.5 7.5a6 6 0 0 1 1.5 3"/>',
  out: '<path d="M12 3v12"/><path d="M7 10l5 5 5-5"/><path d="M4 19h16"/>',
  x: '<path d="M6 6l12 12M18 6L6 18"/>',
  // Mod Menus: a controller-ish pad with a plug-in tab
  mod: '<rect x="3" y="7" width="18" height="11" rx="5"/><path d="M8 10.5v4M6 12.5h4"/><circle cx="15.5" cy="11.5" r=".9" fill="currentColor"/><circle cx="17.5" cy="13.8" r=".9" fill="currentColor"/><path d="M10 7V4.5h4V7"/>',
};
function icon(kind) {
  if (kind === "file" || kind === "folder") return SV(16, 16, ICONS[kind]);
  return SV(16, 16, ICONS[kind] || ICONS.file);
}
// File-type badge icons: a page with a coloured band and a label.
const FT = { zip: ["#6c8cff", "ZIP"], rar: ["#b07cff", "RAR"], "7z": ["#38d6b5", "7Z"], iso: ["#ffb84d", "ISO"], god: ["#4fd8a4", "GOD"], dlc: ["#ff6fb5", "DLC"], tar: ["#ff8a6b", "TAR"], arc: ["#8fa0bf", "ARC"] };
function fileIcon(name, cls = "ft") {
  const ext = String(name).toLowerCase().match(/\.([a-z0-9]+)$/)?.[1] || "";
  const k = name === "god" ? "god" : name === "dlc" ? "dlc" : FT[ext] ? ext : /^(gz|tgz|bz2|xz|lzma|z)$/.test(ext) ? "tar" : N.ARCHIVE_RE.test(name) ? "arc" : "";
  if (!k) return icon("file");
  const [c, label] = FT[k];
  return `<svg class="${cls}" width="20" height="24" viewBox="0 0 30 36" aria-hidden="true"><path d="M5 1.5h14l9 9V32a2.5 2.5 0 0 1-2.5 2.5h-20A2.5 2.5 0 0 1 3 32V4a2.5 2.5 0 0 1 2-2.5z" fill="var(--elevated)" stroke="${c}" stroke-width="1.5"/><path d="M19 1.5V8a2.5 2.5 0 0 0 2.5 2.5H28" fill="none" stroke="${c}" stroke-width="1.5"/><rect x="3" y="19" width="22" height="10" rx="2" fill="${c}"/><text x="14" y="26.6" text-anchor="middle" font-family="Outfit, sans-serif" font-weight="700" font-size="7.4" fill="#0b0e14">${label}</text></svg>`;
}
const LOGO = `<svg viewBox="0 0 64 64" aria-hidden="true"><defs><linearGradient id="lg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#7d97ff"/><stop offset="1" stop-color="#38d6b5"/></linearGradient></defs><rect x="2" y="2" width="60" height="60" rx="16" fill="url(#lg)"/><path d="M16 26l16-8 16 8-16 8z" fill="#fff" fill-opacity=".95"/><path d="M16 26v13l16 8 16-8V26l-16 8z" fill="#0b0e14" fill-opacity=".28"/><path d="M32 34v13" stroke="#fff" stroke-opacity=".7" stroke-width="2"/><path d="M26 12l6-5 6 5" fill="none" stroke="#fff" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
const DROP_ART = `<svg viewBox="0 0 168 112" aria-hidden="true"><defs><linearGradient id="dg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="var(--accent)"/><stop offset="1" stop-color="var(--accent2)"/></linearGradient></defs>
  <ellipse cx="84" cy="104" rx="54" ry="5" fill="var(--border-strong)"/>
  <g class="float"><rect x="18" y="20" width="22" height="27" rx="4" fill="var(--elevated)" stroke="#6c8cff" stroke-width="1.5" transform="rotate(-12 29 33)"/></g>
  <g class="float b"><rect x="128" y="14" width="22" height="27" rx="4" fill="var(--elevated)" stroke="#b07cff" stroke-width="1.5" transform="rotate(10 139 27)"/></g>
  <g class="float c"><circle cx="140" cy="70" r="10" fill="var(--elevated)" stroke="#ffb84d" stroke-width="1.5"/><circle cx="140" cy="70" r="2.5" fill="#ffb84d"/></g>
  <path d="M44 52l40-16 40 16v34l-40 16-40-16z" fill="var(--elevated)" stroke="url(#dg)" stroke-width="2"/>
  <path d="M84 68v34M44 52l40 16 40-16" fill="none" stroke="url(#dg)" stroke-width="2"/>
  <g class="lid"><path d="M44 52l40-16 -6 -14 -40 16z" fill="var(--card)" stroke="url(#dg)" stroke-width="2" stroke-linejoin="round"/></g>
  <path d="M84 6v20M76 18l8 8 8-8" fill="none" stroke="url(#dg)" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/>
</svg>`;

// ---- Settings panel ----
const SETTINGS_EXPORT_KEYS = ["destMode", "destPath", "autoIso", "delIso", "autoDlc", "autoTu", "moveDlc", "libraries", "mods", "modNames", "theme", "notify", "links", "consoles", "xboxLastDest", "xboxDrives", "maxJobs", "nested", "discord", "dlMaxGb", "companionDisc", "releaseCheck", "coverNet", "destPresets", "libSort", "libFilterKind", "libQuery"];
async function exportSettings() {
  const includePw = confirm("Include saved Xbox FTP passwords in the export file?\n\nAnyone with this file can log into your console. Leave them out unless you need a full backup.");
  const data = { version: 1, exportedAt: new Date().toISOString(), settings: {} };
  for (const k of SETTINGS_EXPORT_KEYS) {
    const v = localStorage.getItem("unpack." + k);
    if (v != null) data.settings[k] = v;
  }
  if (!includePw && data.settings.consoles) {
    try {
      const list = JSON.parse(data.settings.consoles);
      data.settings.consoles = JSON.stringify(list.map((c) => ({ ...c, pass: "" })));
    } catch {}
  }
  const out = await N.pickSave("unpack-settings.json", [{ name: "JSON", extensions: ["json"] }], "Export settings");
  if (!out) return;
  window.__exportPickSaveCalled = true;
  try { await N.saveText(out, JSON.stringify(data, null, 2)); } catch (e) { toast("Couldn't save: " + (e?.message || e)); return; }
  toast("Settings exported to " + N.baseName(out));
}
function importSettings() {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = ".json,application/json";
  input.onchange = async () => {
    const f = input.files?.[0];
    if (!f) return;
    try {
      const data = JSON.parse(await f.text());
      const o = data.settings || data;
      // R21 L1/L2: checked values only; saved passwords kept; network opt-ins need a yes;
      // folders on this PC are picked again rather than taken from the file.
      const plan = planImport(o, (k) => localStorage.getItem("unpack." + k));
      for (const [k, v] of Object.entries(plan.apply)) if (SETTINGS_EXPORT_KEYS.includes(k)) localStorage.setItem("unpack." + k, v);
      if (plan.optIns.length) {
        const names = { releaseCheck: "check for new Unpack versions", coverNet: "download cover art", discord: "Discord Rich Presence" };
        if (confirm("This settings file turns on features that use the internet:\n\n- " + plan.optIns.map((k) => names[k] || k).join("\n- ") + "\n\nTurn them on?")) {
          for (const k of plan.optIns) localStorage.setItem("unpack." + k, "1");
        }
      }
      const skipped = plan.dropped.length ? ` Skipped: ${plan.dropped.join(", ")} (invalid, or folders on this PC that you pick again).` : "";
      toast("Settings imported." + skipped + " Re-open dialogs to refresh.");
      render();
    } catch (e) { toast("Could not import: " + e.message); }
  };
  input.click();
}
let settingsOpen = false;
let usbWizardWarn = ""; // last USB wizard warnings, shown amber in Settings
let releaseInfo = null; // { available, latest, url }
let confirmDelIso = false;
let foundRoots = null; // results of "Scan drives", shown for the user to approve one by one
function renderSettings() {
  if (!settingsOpen) return "";
  const t = N.settings.theme;
  const seg = (v, l) => `<button type="button" data-theme-set="${v}" aria-pressed="${t === v}">${l}</button>`;
  return `<div class="overlay" data-settings-overlay><div class="dialog dialog-wide dialog-settings" data-settings role="dialog" aria-modal="true" aria-labelledby="set-h">
    <h3 id="set-h">${icon("gear")} Settings</h3>
    <div class="set-group"><h4>Appearance</h4>
      <div class="set-row"><div class="txt"><strong>Theme</strong><small>System follows your Windows light/dark setting.</small></div>
        <div class="seg" role="group" aria-label="Theme">${seg("system", "System")}${seg("light", "Light")}${seg("dark", "Dark")}</div></div>
      <div class="set-row"><div class="txt"><strong>Notifications</strong><small>Toast when a batch, download or USB copy finishes. Taskbar progress shows during long jobs.</small></div>
        <label class="switch"><input type="checkbox" data-setting="notify" aria-label="Notifications" ${N.settings.notify ? "checked" : ""}/><span></span></label></div>
    </div>
    ${N.IS_DESKTOP ? `<div class="set-group"><h4>Xbox</h4>
      <div class="set-row set-stack"><div class="txt"><strong>Auto-convert extracted ISOs</strong><small>After a batch extracts into its own folder, Xbox 360 / original Xbox .isos inside become either GOD or an extracted <code>default.xex</code> folder next to that archive. Existing folders are never overwritten. When the output is a console <code>Content\\0000000000000000</code> folder (or the drive / <code>Content</code> folder holding it), GOD games go in only as <code>&lt;TitleID&gt;\\00007000\\…</code>: no "(GOD)" or archive-name folders, and archives that already hold a GOD game have just its TitleID folder moved in. Non-Xbox ISOs are skipped and kept.</small></div>
        <div role="group" aria-label="Auto-convert ISOs" class="seg set-mt">
          <button type="button" data-auto-iso="off" aria-pressed="${N.settings.autoIsoMode === "off"}">Off</button>
          <button type="button" data-auto-iso="god" aria-pressed="${N.settings.autoIsoMode === "god"}">GOD</button>
          <button type="button" data-auto-iso="xex" aria-pressed="${N.settings.autoIsoMode === "xex"}">Extracted folder (XEX)</button>
        </div></div>
      <div class="set-row"><div class="txt"><strong>Delete the extracted ISO after a successful convert</strong><small>Saves disk space. Only the ISO copy Unpack just extracted is deleted, and only after the GOD files are checked on disk. Your original archives are never touched.</small></div>
        <label class="switch"><input type="checkbox" data-set-deliso aria-label="Delete extracted ISO after successful convert" ${N.settings.deleteIsoAfterGod ? "checked" : ""}/><span></span></label></div>
      ${confirmDelIso ? `<div class="set-row" role="alert"><div class="txt"><strong class="danger">Turn on ISO deletion?</strong><small>Each extracted ISO is permanently deleted after its GOD copy is fully re-checked. This can't be undone. Never happens with “Replace files”. Note: Unpack can't tell whether a game ISO itself was already damaged (e.g. a bad download of the right size) — the GOD copy would be damaged the same way.</small></div><button class="btn btn-secondary btn-sm" data-deliso-no type="button">Keep ISOs</button><button class="btn btn-primary btn-sm" data-deliso-yes type="button">Yes, delete after convert</button></div>` : ""}
    </div>` : ""}
    ${N.IS_DESKTOP ? `<div class="set-group"><h4>Xbox 360 DLC</h4>
      <div class="set-row"><div class="txt"><strong>Install DLC after extracting</strong><small>DLC packages found in extracted archives are copied into their game: <code>&lt;TitleID&gt;/00000002/</code> next to its <code>00007000</code> folder. Existing files are never overwritten.</small></div>
        <label class="switch"><input type="checkbox" data-set-autodlc aria-label="Install DLC after extracting" ${N.settings.autoDlc ? "checked" : ""}/><span></span></label></div>
      <div class="set-row"><div class="txt"><strong>Install title updates after extracting</strong><small>Title update packages go to <code>&lt;TitleID&gt;/000B0000/</code> beside the game's GOD folder.</small></div>
        <label class="switch"><input type="checkbox" data-set-autotu aria-label="Install title updates after extracting" ${N.settings.autoTu ? "checked" : ""}/><span></span></label></div>
      <div class="set-row"><div class="txt"><strong>Move DLC instead of copying</strong><small>Deletes the original DLC file only after the copy in the game folder is checked.</small></div>
        <label class="switch"><input type="checkbox" data-set-movedlc aria-label="Move DLC instead of copying" ${N.settings.moveDlc ? "checked" : ""}/><span></span></label></div>
      <div class="set-row set-stack"><div class="txt"><strong>Game library folders</strong><small>Where Unpack looks for your games (GOD folders or <code>Content\\0000000000000000</code>). The chosen output folder is always included.</small></div>
        <div class="cmp-list set-mt">${N.settings.libraries.map((l, i) => `<div class="row"><span class="name" title="${escapeHtml(l)}">${escapeHtml(l)}</span><button class="btn btn-quiet" data-lib-remove="${i}" type="button" aria-label="Remove">${SV(14, 14, ICONS.x)}</button></div>`).join("") || `<div class="preview-empty set-empty">No extra folders yet.</div>`}</div>
        ${foundRoots ? `<div class="cmp-list set-mt">${foundRoots.length ? foundRoots.map((r, i) => `<div class="row"><span class="name">${escapeHtml(r)}</span><button class="btn btn-secondary btn-sm" data-root-add="${i}" type="button" ${N.settings.libraries.includes(r) ? "disabled" : ""}>${N.settings.libraries.includes(r) ? "Added" : "Add"}</button></div>`).join("") : `<div class="preview-empty set-empty">No Content folders found at the top of any drive.</div>`}</div>` : ""}
        <div class="actions set-mt"><button class="btn btn-secondary btn-sm" data-lib-add type="button">Add folder…</button><button class="btn btn-secondary btn-sm" data-lib-scan type="button">Scan drives for Content folders</button></div></div>
      <div class="set-row"><div class="txt"><strong>Multi-disc ISO companions</strong><small>When another disc of the same game sits next to an ISO, choose whether to offer it, always add it to the convert queue, or never prompt. Disc 2 alone is never added without disc 1.</small></div>
        <select data-set-companion aria-label="Companion disc mode"><option value="ask" ${N.settings.companionDiscMode === "ask" ? "selected" : ""}>Ask</option><option value="always" ${N.settings.companionDiscMode === "always" ? "selected" : ""}>Always add</option><option value="never" ${N.settings.companionDiscMode === "never" ? "selected" : ""}>Never</option></select></div>
      <div class="set-row"><div class="txt"><strong>Library cover art from the internet</strong><small>Off by default. When on, Unpack downloads cover images in the background after the game list loads (a few at a time, from XboxUnity).</small></div>
        <label class="switch"><input type="checkbox" data-set-covernet aria-label="Download cover art" ${N.settings.coverArtNetwork ? "checked" : ""}/><span></span></label></div>
      <div class="set-row"><div class="txt"><strong>USB wizard</strong><small>Create the standard <code>Content</code> folder layout on a USB drive.</small></div>
        <button class="btn btn-secondary btn-sm" data-usb-wizard type="button">Prepare USB…</button></div>
      ${usbWizardWarn ? `<div class="warn-text set-warn" data-usb-warn role="status">⚠ ${escapeHtml(usbWizardWarn)}</div>` : ""}
    </div>` : ""}
    <div class="set-group"><h4>Downloads</h4>
      <div class="set-row set-stack"><div class="txt"><strong>Max download size (GB)</strong><small>Refuses a transfer larger than this (default 20). Applies to Download from link.</small></div>
        <input type="number" min="1" max="500" step="1" data-set-dlmax class="set-mt" value="${N.settings.dlMaxGb}" aria-label="Max download size in GB" /></div>
    </div>
    <div class="set-group"><h4>Extraction</h4>
      <div class="set-row"><div class="txt"><strong>Open zips inside zips</strong><small>An archive found inside an extracted archive is opened too (up to 3 levels deep). One that would unpack to a suspicious size or more than the drive can hold is left closed with a warning. Off leaves inner archives closed; the summary says how many.</small></div>
        <label class="switch"><input type="checkbox" data-set-nested aria-label="Open zips inside zips" ${N.settings.nested ? "checked" : ""}/><span></span></label></div>
      <div class="set-row"><div class="txt"><strong>Jobs at once</strong><small>How many extractions run side by side. 2 suits most PCs; use 1 if the PC or the USB drive struggles. Jobs aimed at the same folder always take turns.</small></div>
        <select data-set-maxjobs aria-label="Jobs at once">${[1, 2, 3, 4].map((n) => `<option value="${n}" ${N.settings.maxJobs === n ? "selected" : ""}>${n}${n === 2 ? " (recommended)" : n === 1 ? " (one at a time)" : ""}</option>`).join("")}</select></div>
      <div class="set-row"><div class="txt"><strong>Extract as soon as they open</strong><small>Same as the switch on the main screen.</small></div>
        <label class="switch"><input type="checkbox" data-set-autowrite aria-label="Extract as soon as they open" ${autoWrite ? "checked" : ""}/><span></span></label></div>
      <div class="set-row"><div class="txt"><strong>Skip junk files</strong><small>__MACOSX, .DS_Store, Thumbs.db and similar.</small></div>
        <label class="switch"><input type="checkbox" data-set-skipjunk aria-label="Skip junk files" ${skipJunk ? "checked" : ""}/><span></span></label></div>
    </div>
    ${N.IS_DESKTOP ? `<div class="set-group"><h4>Discord</h4>
      <div class="set-row"><div class="txt"><strong>Show Unpack on Discord (Rich Presence)</strong><small>Shows which screen you are on and how many jobs are running, never file names or folders. Talks only to the Discord app on this PC; Unpack makes no network connection for it. Off by default; off clears it right away.</small></div>
        <label class="switch"><input type="checkbox" data-set-discord aria-label="Show Unpack on Discord" ${N.settings.discordPresence ? "checked" : ""}/><span></span></label></div>
    </div>` : ""}
    <div class="set-group"><h4>Updates &amp; backup</h4>
      <div class="set-row"><div class="txt"><strong>Check for updates on startup</strong><small>Opt-in. Compares this build to the latest release (network).</small></div>
        <label class="switch"><input type="checkbox" data-set-release aria-label="Check for updates" ${N.settings.releaseCheck ? "checked" : ""}/><span></span></label></div>
      <div class="set-row"><div class="txt"><strong>Export / import settings</strong><small>One JSON file. Console FTP passwords are optional — exporting them is warned in the dialog.</small></div>
        <div class="actions"><button class="btn btn-secondary btn-sm" data-set-export type="button">Export…</button><button class="btn btn-secondary btn-sm" data-set-import type="button">Import…</button></div></div>
    </div>
    <div class="actions set-done"><button class="btn btn-primary" data-settings-close type="button">Done</button></div>
  </div></div>`;
}
function bindSettings() {
  app.querySelector("[data-action=settings]")?.addEventListener("click", () => { settingsOpen = true; render(); app.querySelector("[data-settings-close]")?.focus(); });
  const root = app.querySelector("[data-settings]");
  if (!root) return;
  const close = () => { settingsOpen = false; confirmDelIso = false; foundRoots = null; render(); app.querySelector("[data-action=settings]")?.focus(); };
  root.querySelector("[data-settings-close]")?.addEventListener("click", close);
  app.querySelector("[data-settings-overlay]")?.addEventListener("click", (e) => { if (e.target === e.currentTarget) close(); });
  root.addEventListener("keydown", (e) => { if (e.key === "Escape") close(); });
  root.querySelector("[data-setting=notify]")?.addEventListener("change", (e) => { N.settings.notify = e.target.checked; });
  root.querySelectorAll("[data-theme-set]").forEach((b) => b.addEventListener("click", () => { N.settings.theme = b.dataset.themeSet; window.__applyTheme?.(); render(); }));
  root.querySelector("[data-set-deliso]")?.addEventListener("change", (e) => {
    if (e.target.checked) { e.target.checked = false; confirmDelIso = true; return render(); }
    N.settings.deleteIsoAfterGod = false;
  });
  root.querySelector("[data-set-autodlc]")?.addEventListener("change", (e) => { N.settings.autoDlc = e.target.checked; });
  root.querySelector("[data-set-autotu]")?.addEventListener("change", (e) => { N.settings.autoTu = e.target.checked; });
  root.querySelector("[data-set-movedlc]")?.addEventListener("change", (e) => { N.settings.moveDlc = e.target.checked; });
  root.querySelector("[data-set-companion]")?.addEventListener("change", (e) => { N.settings.companionDiscMode = e.target.value; });
  root.querySelector("[data-set-covernet]")?.addEventListener("change", (e) => { N.settings.coverArtNetwork = e.target.checked; if (!e.target.checked) libCoverRun += 1; else if (libraryView) loadLibraryCovers(); });
  root.querySelector("[data-usb-wizard]")?.addEventListener("click", async () => {
    const rootPath = await N.pickFolder("Pick the USB drive root (top folder)");
    if (!rootPath) return;
    try {
      const r = await N.invoke("usb_prepare", { root: rootPath });
      const made = r.created?.length ? `Created ${r.created.length} folder${r.created.length === 1 ? "" : "s"}.` : "Folders already there.";
      const msg = `${r.fat ? "FAT32 ✓" : `File system: ${r.fs_type}`} · ${made}`;
      // Warnings are advice, not failures: amber note in Settings, not the red error banner.
      usbWizardWarn = r.warnings?.length ? [msg, ...r.warnings, ...(r.big_files || []).slice(0, 5)].join("\n") : "";
      if (!usbWizardWarn) toast(msg);
      render();
    } catch (err) { toast(String(err?.message || err)); }
  });
  root.querySelector("[data-set-release]")?.addEventListener("change", (e) => { N.settings.releaseCheck = e.target.checked; });
  root.querySelector("[data-set-export]")?.addEventListener("click", () => exportSettings());
  root.querySelector("[data-set-import]")?.addEventListener("click", () => importSettings());
  root.querySelectorAll("[data-lib-remove]").forEach((b) => b.addEventListener("click", () => { const l = N.settings.libraries; l.splice(Number(b.dataset.libRemove), 1); N.settings.libraries = l; render(); }));
  root.querySelector("[data-lib-add]")?.addEventListener("click", async () => { const p = await N.pickFolder("Add a game library folder"); if (p) { N.settings.libraries = [...N.settings.libraries, p]; retryDlcNotFound(true); render(); } });
  root.querySelector("[data-lib-scan]")?.addEventListener("click", async () => { foundRoots = await N.invoke("dlc_scan_drives").catch(() => []); render(); });
  root.querySelectorAll("[data-root-add]").forEach((b) => b.addEventListener("click", () => { const r = foundRoots[Number(b.dataset.rootAdd)]; if (r) { N.settings.libraries = [...N.settings.libraries, r]; retryDlcNotFound(true); render(); } }));
  root.querySelector("[data-deliso-yes]")?.addEventListener("click", () => { N.settings.deleteIsoAfterGod = true; confirmDelIso = false; render(); });
  root.querySelector("[data-deliso-no]")?.addEventListener("click", () => { confirmDelIso = false; render(); });
  root.querySelector("[data-set-autowrite]")?.addEventListener("change", (e) => { const m = app.querySelector(".desk [data-setting=autoWrite]"); if (m) { m.checked = e.target.checked; m.dispatchEvent(new Event("change")); } });
  root.querySelector("[data-set-skipjunk]")?.addEventListener("change", (e) => { const m = app.querySelector(".desk [data-setting=skipJunk]"); if (m) { m.checked = e.target.checked; m.dispatchEvent(new Event("change")); } });
  root.querySelector("[data-set-dlmax]")?.addEventListener("change", (e) => { N.settings.dlMaxGb = e.target.value; });
  root.querySelector("[data-set-nested]")?.addEventListener("change", (e) => { N.settings.nested = e.target.checked; });
  root.querySelector("[data-set-maxjobs]")?.addEventListener("change", (e) => { N.settings.maxJobs = e.target.value; pump(); });
  root.querySelector("[data-set-discord]")?.addEventListener("change", async (e) => {
    N.settings.discordPresence = e.target.checked;
    presenceKey = "";
    await N.invoke("discord_set_enabled", { enabled: e.target.checked }).catch(() => {});
    if (e.target.checked) updatePresence();
  });
}

function renderTree(nodes, depth = 0) {
  return nodes
    .map((n) => {
      if (n.type === "dir") {
        return `<div>
          <div class="row row-indent" data-depth="${depth}">${icon("folder")}<span class="name">${escapeHtml(n.name)}</span></div>
          ${renderTree(n.children || [], depth + 1)}
        </div>`;
      }
      const sel = selectedPath === n.path ? " selected" : "";
      const tag = n.file?.deferred ? `<span class="size">ISO</span>` : "";
      return `<button class="row${sel} row-indent" data-file="${encodeURIComponent(n.path)}" data-depth="${depth}">${fileIcon(n.name)}<span class="name">${escapeHtml(n.name)}</span>${tag}<span class="size">${formatBytes(n.size)}</span></button>`;
    })
    .join("");
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function renderPreview(file) {
  if (!file) return `<div class="preview-empty">Select a file to preview.</div>`;
  if (file.native && !file.data) {
    const name = file.path.split("/").pop();
    const head = `<div class="preview-head"><div><strong>${escapeHtml(name)}</strong><span>${escapeHtml(file.path)} · ${formatBytes(file.size)}</span></div><button class="btn btn-secondary" data-action="save-one">Save file</button></div>`;
    if (file.previewError) return `${head}<div class="preview-empty">${escapeHtml(file.previewError)}</div>`;
    if ((isText(file.path) || isImage(file.path)) && file.size <= 4 * 1024 * 1024) {
      queueMicrotask(() => previewNative(file));
      return `${head}<div class="preview-empty"><span class="spin">◌</span> Loading preview…</div>`;
    }
    return `${head}<div class="preview-empty">No preview for this file type. Save it, or extract the archive.</div>`;
  }
  const name = file.path.split("/").pop();
  const extra = file.deferred ? " · large file, save to a folder" : "";
  const head = `<div class="preview-head"><div><strong>${escapeHtml(name)}</strong><span>${escapeHtml(file.path)} · ${formatBytes(file.size)}${extra}</span></div><button class="btn btn-secondary" data-action="save-one">Save file</button></div>`;
  if (file.deferred || !file.data) {
    return `${head}<div class="preview-empty">This ${escapeHtml(name)} is too large to preview. Use Extract to folder — Unpack will stream it to disk without loading the whole ISO into memory.</div>`;
  }
  if (isImage(file.path)) {
    const url = URL.createObjectURL(new Blob([file.data]));
    return `${head}<div class="preview-body"><img src="${url}" alt="" /></div>`;
  }
  if (isText(file.path) && file.size < 400000) {
    let text = "";
    try {
      text = strFromU8(file.data);
    } catch {
      text = new TextDecoder("utf-8", { fatal: false }).decode(file.data);
    }
    return `${head}<div class="preview-body"><pre>${escapeHtml(text)}</pre></div>`;
  }
  return `${head}<div class="preview-empty">No preview for this file type. Save it instead.</div>`;
}

function downloadBlob(blob, name) {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 2000);
}

function downloadFile(file) {
  if (file.native) return saveEntryNative(file);
  if (file.deferred || !file.data) {
    toast("Use Extract to folder for ISOs and other large files.");
    const job = activeJob();
    if (job) saveToFolder(job);
    return;
  }
  const name = file.path.split("/").pop() || "file";
  downloadBlob(new Blob([file.data]), name);
}

function downloadAllAsZip(job) {
  const payload = {};
  for (const f of job.files) {
    if (f.data && !f.deferred) payload[f.path] = f.data;
  }
  if (!Object.keys(payload).length) {
    toast("The remaining files are too large for a ZIP download. Use Extract to folder.");
    return;
  }
  const zipped = zipSync(payload);
  downloadBlob(new Blob([zipped], { type: "application/zip" }), archiveStem(job.name) + "-extracted.zip");
}

async function writeChunks(writable, data) {
  if (!data || !data.byteLength) {
    await writable.write(new Blob([]));
    return;
  }
  const chunk = 8 * 1024 * 1024;
  if (data.byteLength > chunk) {
    for (let i = 0; i < data.byteLength; i += chunk) {
      await writable.write(new Blob([data.subarray(i, Math.min(i + chunk, data.byteLength))]));
    }
  } else {
    await writable.write(new Blob([data]));
  }
}

async function streamZipMember(sourceFile, entryPath, writable, onStatus) {
  let resolve, reject;
  const done = new Promise((res, rej) => { resolve = res; reject = rej; });
  let found = false;
  const writes = [];
  const unzipper = new Unzip((entry) => {
    if (sanitizePath(entry.name) !== entryPath) return;
    found = true;
    entry.ondata = (err, dat, final) => {
      if (err) return reject(err);
      writes.push(writable.write(dat));
      if (final) Promise.all(writes).then(resolve).catch(reject);
    };
    entry.start();
  });
  unzipper.register(UnzipInflate);
  const size = Number(sourceFile.size) || 0;
  for (let off = 0; off < size; off += READ_CHUNK) {
    onStatus?.("Writing " + entryPath + "… " + Math.round((off / size) * 100) + "%");
    const part = await readSlice(sourceFile, off, Math.min(off + READ_CHUNK, size));
    unzipper.push(part, off + READ_CHUNK >= size);
  }
  if (!found) throw new Error("Could not find " + entryPath + " in the ZIP");
  await done;
}

async function stream7zMember(job, entryPath, writable, onStatus) {
  const buf = job.bytes;
  if (!buf) throw new Error("Archive is no longer in memory. Drop it again.");
  onStatus?.("Extracting " + entryPath + "…");
  const sz = await getSevenZip(onStatus);
  rmVirtual(sz, "/in");
  rmVirtual(sz, "/out");
  sz.FS.writeFile("/in/archive.7z", buf);
  try { sz.callMain(["x", "/in/archive.7z", "-o/out", "-y", "-t7z", "-spd", "-p" + (job.password || "\u0001no-password"), "--", entryPath]); } catch {}
  const outPath = "/out/" + entryPath;
  const stream = sz.FS.open(outPath, "r");
  const tmp = new Uint8Array(READ_CHUNK);
  while (true) {
    const n = sz.FS.read(stream, tmp, 0, tmp.length);
    if (!n) break;
    await writable.write(tmp.subarray(0, n));
  }
  sz.FS.close(stream);
  rmVirtual(sz, "/in");
  rmVirtual(sz, "/out");
}

function safeSegment(name) {
  let s = String(name || "file").replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_").replace(/[. ]+$/g, "").trim();
  if (!s || s === "." || s === "..") s = "file";
  if (/^(con|prn|aux|nul|com[0-9]|lpt[0-9]|clock\$|conin\$|conout\$)$/i.test(s)) s = "_" + s;
  if (s.length > 72) s = s.slice(0, 64) + "-" + s.slice(-6);
  return s;
}

function outputEntries(job) {
  const list = job.files.filter((f) => !(skipJunk && isJunk(f.path)));
  const stem = archiveStem(job.name).toLowerCase();
  const roots = new Set(list.map((f) => (f.path.split("/")[0] || "").toLowerCase()).filter(Boolean));
  const stripRoot = roots.size === 1 && [...roots][0] === stem;
  return list.map((file) => {
    const raw = stripRoot ? file.path.split("/").slice(1).join("/") : file.path;
    const parts = (raw || file.path.split("/").pop() || "file").split("/").filter(Boolean).map(safeSegment);
    return { file, parts };
  });
}

async function withRetry(fn) {
  try {
    return await fn();
  } catch (err) {
    if (err?.name !== "NotFoundError" && !/could not be found/i.test(String(err?.message || err))) throw err;
    await new Promise((r) => setTimeout(r, 50));
    return await fn();
  }
}
async function resolveDestName(root, stem) {
  if (collision === "replace") return stem;
  const exists = async (name) => {
    try { await root.getDirectoryHandle(name); return true; } catch { return false; }
  };
  if (collision === "skip") return (await exists(stem)) ? null : stem;
  let name = stem;
  let n = 2;
  while (await exists(name)) {
    name = stem + "-" + n;
    n += 1;
  }
  return name;
}

async function writeJobToDirectory(job, root) {
  const resolved = await resolveDestName(root, archiveStem(job.name));
  if (!resolved) {
    job.phase = "skipped";
    job.progress = 100;
    job.status = "Already in this folder — skipped";
    render();
    return 0;
  }
  const folderName = safeSegment(resolved);
  const dest = await withRetry(() => root.getDirectoryHandle(folderName, { create: true }));
  const dirCache = new Map([["", dest]]);
  async function ensureDir(parts) {
    const path = parts.join("/");
    if (dirCache.has(path)) return dirCache.get(path);
    const parent = await ensureDir(parts.slice(0, -1));
    const handle = await withRetry(() => parent.getDirectoryHandle(parts.at(-1), { create: true }));
    dirCache.set(path, handle);
    return handle;
  }
  let written = 0;
  let shortened = 0;
  job.phase = "saving";
  job.status = "Writing to " + folderName + "…";
  job.progress = 8;
  job.error = "";
  render();
  const list = outputEntries(job);
  const skipped = job.files.length - list.length;
  for (const entry of list) {
    if (job.cancelled) throw new Error("Cancelled");
    const file = entry.file;
    let parts = entry.parts.slice();
    let filename = parts.pop() || "file";
    let dir = dest;
    try {
      dir = parts.length ? await ensureDir(parts) : dest;
    } catch (err) {
      if (err?.name !== "NotFoundError" && !/could not be found/i.test(String(err?.message || err))) throw err;
      filename = safeSegment(parts.join("_") + "_" + filename).slice(0, 80);
      parts = [];
      dir = dest;
      shortened += 1;
    }
    const candidates = [filename];
    if (/\.iso$/i.test(filename)) candidates.push(filename.replace(/\.iso$/i, ".img"), filename + ".img");
    let writable = null;
    let usedName = filename;
    let lastErr = null;
    for (const name of candidates) {
      try {
        const handle = await withRetry(() => dir.getFileHandle(name, { create: true }));
        writable = await withRetry(() => handle.createWritable());
        usedName = name;
        break;
      } catch (err) {
        lastErr = err;
      }
    }
    if (!writable && parts.length) {
      try {
        const flat = safeSegment(parts.join("_") + "_" + filename).slice(0, 80);
        const handle = await dest.getFileHandle(flat, { create: true });
        writable = await handle.createWritable();
        usedName = flat;
        shortened += 1;
      } catch (err) {
        lastErr = err;
      }
    }
    if (!writable) throw lastErr || new Error("Could not save " + filename);
    try {
      if (file.deferred || !file.data) {
        if (job.format === "zip") await streamZipMember(job.sourceFile || job.file, file.path, writable, (msg) => { job.status = msg; render(); });
        else if (job.format === "7z") await stream7zMember(job, file.path, writable, (msg) => { job.status = msg; render(); });
        else if (job.format === "rar") {
          await writable.close();
          writable = null;
          (job.warnings || (job.warnings = [])).push(file.path + " is too large to extract from RAR in the browser. Use WinRAR/7-Zip for that ISO.");
          continue;
        } else {
          throw new Error("Cannot stream " + file.path);
        }
      } else {
        await writeChunks(writable, file.data);
      }
    } finally {
      if (writable) await writable.close();
    }
    if (usedName !== filename) (job.warnings || (job.warnings = [])).push(file.path + " was saved as " + usedName);
    written += 1;
    job.progress = 8 + Math.round((written / Math.max(list.length, 1)) * 92);
    render();
  }
  job.phase = "saved";
  job.progress = 100;
  job.status = `Saved ${written} files to ${folderName}/` + (skipped ? ` · ${skipped} junk skipped` : "") + (shortened ? ` · ${shortened} path shortened` : "");
  return written;
}

function openDb() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open("unpack-desk", 1);
    req.onupgradeneeded = () => req.result.createObjectStore("kv");
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}
async function idbSet(key, value) {
  const db = await openDb();
  await new Promise((resolve, reject) => {
    const tx = db.transaction("kv", "readwrite");
    tx.objectStore("kv").put(value, key);
    tx.oncomplete = resolve;
    tx.onerror = () => reject(tx.error);
  });
}
async function idbGet(key) {
  const db = await openDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction("kv", "readonly");
    const req = tx.objectStore("kv").get(key);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}
async function rememberDest(handle) {
  destHandle = handle;
  destName = handle?.name || "";
  try { await idbSet("dest", handle); await idbSet("destName", destName); } catch {}
}
async function chooseFolder() {
  if (!folderCapable) {
    toast("This browser can't pick a folder. Use Download as ZIP.");
    return null;
  }
  const handle = await window.showDirectoryPicker({ mode: "readwrite" });
  await rememberDest(handle);
  render();
  return handle;
}
async function restoreDesk() {
  try {
    collision = (await idbGet("collision")) || "number";
    autoWrite = (await idbGet("autoWrite")) !== false;
    skipJunk = (await idbGet("skipJunk")) !== false;
    destName = (await idbGet("destName")) || "";
    const handle = await idbGet("dest");
    if (handle && handle.queryPermission) {
      const perm = await handle.queryPermission({ mode: "readwrite" });
      if (perm === "granted") {
        destHandle = handle;
        destName = handle.name || destName;
      } else destName = destName || handle.name || "";
    }
  } catch {}
  render();
}

async function saveToFolder(job, root) {
  if (!folderCapable) {
    downloadAllAsZip(job);
    toast("This browser can't pick a folder. Downloaded a zip instead.");
    return;
  }
  try {
    const dir = root || destHandle || await chooseFolder();
    if (!dir) return;
    await writeJobToDirectory(job, dir);
    if (!root) toast(job.phase === "skipped" ? "Already extracted. Skipped." : "Saved to " + destName + ".");
    render();
  } catch (err) {
    if (err?.name === "AbortError" || /cancelled/i.test(err?.message || "")) return;
    job.phase = "failed";
    job.error = err?.message || String(err);
    render();
  }
}

async function saveAllToFolder() {
  const ready = jobs.filter((j) => j.files.length && j.phase !== "saving");
  if (!ready.length) {
    toast("Nothing is ready to write yet.");
    return;
  }
  if (!folderCapable) {
    ready.forEach(downloadAllAsZip);
    toast("This browser can't pick a folder. Downloaded ZIPs instead.");
    return;
  }
  try {
    const root = destHandle || await chooseFolder();
    if (!root) return;
    let total = 0;
    let failed = 0;
    let skipped = 0;
    for (const job of ready) {
      if (job.cancelled) continue;
      activeId = job.id;
      render();
      try {
        const n = await writeJobToDirectory(job, root);
        if (job.phase === "skipped") skipped += 1;
        else total += n;
      } catch (err) {
        if (err?.name === "AbortError") return;
        job.phase = "failed";
        job.error = err?.message || String(err);
        failed += 1;
        render();
      }
    }
    toast(failed ? `Wrote ${total} files. ${failed} failed.` : `Wrote ${total} files` + (skipped ? `, skipped ${skipped}` : "") + ".");
    render();
  } catch (err) {
    if (err?.name === "AbortError") return;
    toast(err?.message || String(err));
  }
}

function clearAll() {
  if (!jobs.length) return;
  for (const j of jobs) if (j.native && (j.phase === "saving" || j.phase === "queued")) cancelJob(j);
  waiting.length = 0;
  jobs = [];
  activeId = null;
  selectedPath = null;
  query = "";
  fileInput.value = "";
  render();
  toast("All archives removed.");
}

function activeJob() {
  return jobs.find((j) => j.id === activeId) || null;
}

async function addFiles(list) {
  const incoming = [...list].filter(isArchive);
  if (!incoming.length) {
    toast("Drop a .zip, .rar, or .7z file.");
    return;
  }
  for (const file of incoming) {
    const job = {
      id: uid(),
      name: file.name,
      size: file.size,
      sourceFile: new File([file.slice(0, file.size)], file.name, { type: file.type || "application/octet-stream" }),
      file,
      bytes: null,
      files: [],
      warnings: [],
      format: "",
      phase: "opening",
      status: "Queued",
      progress: 4,
      error: "",
      password: "",
      cancelled: false,
    };
    jobs = [job, ...jobs];
    activeId = job.id;
    selectedPath = null;
    query = "";
    render();
    await runExtract(job);
  }
}

async function runExtract(job, password) {
  if (job.native) return openNative(job);
  try {
    job.status = password ? "Unlocking archive…" : "Opening…";
    job.phase = "opening";
    job.progress = 18;
    job.error = "";
    render();
    const { files, format } = await Promise.race([
      extractArchive(job, password, (msg) => {
        if (job.cancelled) return;
        job.status = msg;
        job.progress = Math.min(90, (job.progress || 10) + 8);
        render();
      }),
      new Promise((_, reject) => setTimeout(() => reject(new Error("Timed out after 5 minutes. Big ISOs inside archives can take a while — try again or extract that archive on the desktop.")), 300000)),
    ]);
    if (job.cancelled) {
      job.phase = "failed";
      job.status = "Cancelled";
      render();
      return;
    }
    const usable = files.filter((f) => !isHidden(f.path));
    job.files = usable.length ? usable : files;
    job.format = format;
    job.phase = "ready";
    job.progress = 100;
    job.status = `${job.files.length} files · ${formatBytes(job.files.reduce((n, f) => n + f.size, 0))}`;
    if (!job.files.length) {
      job.phase = "failed";
      job.error = "The archive opened, but no files were found.";
    } else if (job.warnings?.length) job.status += " · " + job.warnings.length + " noted";
    render();
    if (destHandle && autoWrite && job.files.length && !job.cancelled) {
      try { await writeJobToDirectory(job, destHandle); }
      catch (err) {
        if (err?.name !== "AbortError") {
          job.phase = "failed";
          job.error = err?.message || String(err);
        }
      }
      render();
    }
  } catch (err) {
    if (looksEncrypted(err) && !password) {
      job.status = "Password required";
      job.progress = 0;
      render();
      const pw = await askPassword(job.name);
      if (pw) {
        job.password = pw;
        return runExtract(job, pw);
      }
      job.phase = "failed";
      job.error = "Skipped — password not entered. The rest of the queue continues.";
      render();
      return;
    }
    job.progress = 0;
    job.phase = "failed";
    job.status = "Failed";
    job.error = friendlyError(err);
    render();
  }
}

function friendlyError(err) {
  const raw = String(err?.message || err || "Could not extract this archive.");
  if (/could not be found at the time an operation was processed|NotFoundError/i.test(raw)) {
    return "Windows could not create that path. It is usually too long — this pack repeats the game name inside the archive. Unpack now skips that duplicate folder and shortens the rest. Write this archive again.";
  }
  if (isNotReadable(err) || raw === "NOT_READABLE") {
    return "Opera cannot read this archive in one piece (common when an ISO makes the file bigger than ~2 GB). The new page reads it in slices and streams the ISO to a folder. If you still see this, copy the archive to your Desktop and drop that copy, then use Extract to folder.";
  }
  if (/ARCHIVE_TOO_LARGE/i.test(raw)) {
    return "This archive is larger than the browser can load at once (about 1.5 GB). ZIP files that big can still stream an ISO to a folder. RAR/7Z that large need 7-Zip or WinRAR on the desktop.";
  }
  if (looksEncrypted(err)) return "This archive is password-protected. Enter the password if you have one. Encrypted RAR/7Z support is limited in the browser.";
  if (/failed to fetch|built-in extractor/i.test(raw)) return "Could not load the built-in RAR/7Z engine. Try reinstalling Unpack.";
  if (/not compiled for this environment|worker/i.test(raw)) return "This browser blocked the old extractor worker. Reload this updated page and try the RAR/7Z again.";
  if (/out of memory|maximum call stack|array buffer|allocation/i.test(raw)) return "This archive (or an ISO inside it) is too large for the browser to hold in memory. Extract it with 7-Zip or WinRAR on the desktop.";
  return raw;
}

function renderPassword() {
  if (!passwordPrompt) return "";
  return `<div class="overlay"><form class="dialog" data-pw>
    <h3>Password needed</h3>
    <p>${passwordPrompt.wrong ? `<span class="danger">That password didn't work.</span> ` : ""}${escapeHtml(passwordPrompt.filename)} is encrypted. Nothing is uploaded — the password stays on this computer.</p>
    <input id="pw" type="password" autocomplete="off" placeholder="Archive password" />
    <div class="actions">
      <button class="btn btn-primary" type="submit">Unlock</button>
      <button class="btn btn-secondary" type="button" data-pw-cancel>Cancel</button>
    </div>
  </form></div>`;
}

function renderNativeDesk() {
  const mode = settings().destMode;
  const path = settings().destPath;
  const ready = jobs.filter((j) => j.native && j.files.length && j.phase === "ready").length;
  const where =
    mode === "beside" ? "Next to each archive" : mode === "folder" && path ? path : mode === "folder" ? "No folder chosen yet" : "Ask every time";
  const note =
    mode === "beside" ? "Each archive gets its own folder beside it." : mode === "folder" ? "Every archive gets its own folder in here." : "You'll pick a folder for each extraction.";
  return `<section class="desk">
    <div class="dest">
      <div class="dest-mark${mode !== "ask" && (mode !== "folder" || path) ? " on" : ""}"></div>
      <div class="dest-copy">
        <strong>${escapeHtml(where)}</strong>
        <span>${escapeHtml(note)}${autoWrite && mode === "folder" && path ? " New archives extract here as soon as they open." : ""}</span>
      </div>
      <div class="controls">
        <label>Extract to <select data-setting="destMode">
          <option value="beside" ${mode === "beside" ? "selected" : ""}>Next to archive</option>
          <option value="folder" ${mode === "folder" ? "selected" : ""}>A chosen folder</option>
          <option value="ask" ${mode === "ask" ? "selected" : ""}>Ask every time</option>
        </select></label>
        <button class="btn btn-secondary" data-action="choose-folder" type="button">${path ? "Change folder" : "Choose folder"}</button>
        ${jobs.length ? `<button class="btn btn-secondary" data-action="save-all" ${ready ? "" : "disabled"}>Extract remaining (${ready})</button>` : ""}
        ${jobs.some((j) => j.phase === "saving" || j.phase === "queued" || (j.gods || []).some((g) => g.phase === "saving" || g.phase === "queued")) ? `<button class="btn btn-quiet" data-action="cancel" type="button">Cancel all</button>` : ""}
      </div>
    </div>
    <div class="controls">
      <label>If it already exists <select data-setting="collision">
        <option value="number" ${collision === "number" ? "selected" : ""}>Add a number</option>
        <option value="replace" ${collision === "replace" ? "selected" : ""}>Replace files</option>
        <option value="skip" ${collision === "skip" ? "selected" : ""}>Skip archive</option>
      </select></label>
      <label><input type="checkbox" data-setting="autoWrite" ${autoWrite ? "checked" : ""}/> Extract as soon as they open</label>
      <label><input type="checkbox" data-setting="skipJunk" ${skipJunk ? "checked" : ""}/> Skip junk files</label>
      <label title="Off / GOD / extracted XEX folder">Auto ISO <select data-setting="autoIsoMode" aria-label="Auto-convert extracted ISOs">
        <option value="off" ${N.settings.autoIsoMode === "off" ? "selected" : ""}>Off</option>
        <option value="god" ${N.settings.autoIsoMode === "god" ? "selected" : ""}>→ GOD</option>
        <option value="xex" ${N.settings.autoIsoMode === "xex" ? "selected" : ""}>→ Extracted folder</option>
      </select></label>
    </div>
    ${renderQueue()}
  </section>`;
}

function renderQueue() {
  const manual = dlcManual?.dlcRows?.length ? `<div class="qlist"><div class="qhead"><span>Install DLC · ${dlcManual.dlcRows.length}</span><button class="btn btn-quiet" data-action="clear-dlc" type="button">Clear</button></div>${renderDlcRows(dlcManual.dlcRows)}</div>` : "";
  if (!jobs.length) return manual;
  return manual + renderQueueJobs();
}
function renderQueueJobs() {
  const active = jobs.filter((j) => j.phase === "saving" || j.phase === "queued").length;
  const paused = N.settings.queuePaused;
  const waitIds = new Set(waiting.map((w) => w.job.id));
  const hasWait = waiting.length > 0 || jobs.some((j) => j.phase === "queued");
  return `<div class="qlist">
    <div class="qhead"><span>Queue · ${jobs.length} archive${jobs.length === 1 ? "" : "s"}${active ? ` · ${active} working` : ""}${paused ? " · paused" : ""}</span>
      <span class="actions">${hasWait ? `<button class="btn btn-quiet btn-sm" data-queue-pause type="button">${paused ? "Resume" : "Pause"}</button>` : ""}${jobs.some((j) => j.phase === "saved" || j.phase === "skipped" || j.phase === "failed") ? `<button class="btn btn-quiet" data-action="clear-done" type="button">Clear finished</button>` : ""}</span></div>
    ${jobs
      .map(
        (j) => `<div class="qrow${j.id === activeId ? " on" : ""}">
        <button class="qname" data-open="${escapeHtml(j.id)}" type="button"><span class="dot ${escapeHtml(j.phase || "opening")}"></span>${fileIcon(j.name)}<span class="name">${escapeHtml(j.name)}</span></button>
        <div class="qbar"><div data-job-bar="${escapeHtml(j.id)}" class="qbar-fill"></div></div>
        <span class="qstatus" data-job-status="${escapeHtml(j.id)}">${escapeHtml(j.status || j.phase)}</span>
        ${j.phase === "queued" && waitIds.has(j.id) ? `<span class="actions"><button class="btn btn-quiet btn-sm" data-queue-up="${escapeHtml(j.id)}" type="button" title="Move up">↑</button><button class="btn btn-quiet btn-sm" data-queue-down="${escapeHtml(j.id)}" type="button" title="Move down">↓</button><button class="btn btn-quiet btn-sm" data-queue-front="${escapeHtml(j.id)}" type="button" title="Move to front">⤒</button><button class="btn btn-quiet btn-sm" data-cancel-job="${escapeHtml(j.id)}" type="button">Cancel</button></span>` : j.phase === "saving" || j.phase === "queued" ? `<button class="btn btn-quiet" data-cancel-job="${escapeHtml(j.id)}" type="button">Cancel</button>` : `<span></span>`}
      </div>${(j.gods || []).map((g) => `<div class="qrow sub">
        <span class="qname" title="${escapeHtml(g.error || g.path)}"><span class="dot ${escapeHtml(g.phase)}"></span>${fileIcon(g.phase === "saved" ? "god" : g.name)}<span class="name">${escapeHtml(g.name)}</span></span>
        <div class="qbar"><div data-job-bar="${escapeHtml(g.id)}" class="qbar-fill"></div></div>
        <span class="qstatus${g.phase === "failed" ? " danger" : ""}" data-job-status="${escapeHtml(g.id)}" data-status-kind="god" title="${escapeHtml(g.error || g.warning || g.status)}">${escapeHtml(godStatusText(g))}</span>
        ${g.phase === "saving" || g.phase === "queued" ? `<button class="btn btn-quiet" data-cancel-gods="${escapeHtml(j.id)}" type="button">Cancel</button>` : `<span></span>`}
      </div>`).join("")}${renderDlcRows(j.dlcRows)}`
      )
      .join("")}
  </div>`;
}

function renderDesk() {
  if (N.IS_DESKTOP) return renderNativeDesk();
  const ready = jobs.filter((j) => j.files.length && j.phase !== "saved" && j.phase !== "skipped").length;
  const connected = !!destHandle;
  return `<section class="desk">
    <div class="dest">
      <div class="dest-mark${connected ? " on" : ""}"></div>
      <div class="dest-copy">
        <strong>${connected ? escapeHtml(destName) : destName ? "Reconnect " + escapeHtml(destName) : "No destination"}</strong>
        <span>${connected ? (autoWrite ? "New archives write here as they finish." : "Folder remembered. Write when you choose.") : "Pick a folder once. Unpack keeps it for the next batch."}</span>
      </div>
      <div class="controls">
        <button class="btn btn-primary" data-action="choose-folder" type="button">${connected ? "Change folder" : "Choose folder"}</button>
        ${jobs.length ? `<button class="btn btn-secondary" data-action="save-all" ${ready ? "" : "disabled"}>${folderCapable ? "Write remaining" : "Download all"} (${ready})</button>` : ""}
        ${jobs.some((j) => j.phase === "opening" || j.phase === "saving") ? `<button class="btn btn-quiet" data-action="cancel" type="button">Cancel</button>` : ""}
      </div>
    </div>
    <div class="controls">
      <label>If the folder exists <select data-setting="collision">
        <option value="number" ${collision === "number" ? "selected" : ""}>Add a number</option>
        <option value="replace" ${collision === "replace" ? "selected" : ""}>Replace files</option>
        <option value="skip" ${collision === "skip" ? "selected" : ""}>Skip archive</option>
      </select></label>
      <label><input type="checkbox" data-setting="autoWrite" ${autoWrite ? "checked" : ""}/> Write as they finish</label>
      <label><input type="checkbox" data-setting="skipJunk" ${skipJunk ? "checked" : ""}/> Skip junk files</label>
    </div>
    ${jobs.length ? `<div class="queue">${jobs.map((j) => `<button class="chip${j.id === activeId ? " on" : ""}" data-open="${j.id}" type="button"><span class="dot ${j.phase || "opening"}"></span>${escapeHtml(j.name)} · ${escapeHtml(j.phase || "opening")}</button>`).join("")}</div>` : ""}
  </section>`;
}

function render() {
  const active = activeJob();
  app.innerHTML = `
    <header class="header">
      <div class="brand">
        <div class="logo">${LOGO}</div>
        <div>
          <h1>Unpack</h1>
          <p>Local archive desk</p>
        </div>
      </div>
      <div class="badges">
        ${N.IS_DESKTOP && nativeOk ? `<button class="btn btn-secondary btn-sm" data-action="create" type="button" title="Make a ZIP or 7Z from files and folders (Ctrl+N)">${icon("pack")} Create archive</button>` : ""}
        ${N.IS_DESKTOP ? `<button class="btn btn-secondary btn-sm" data-action="library" type="button" title="Your games, their DLC and Copy to USB (Ctrl+L)">${icon("folder")} Library</button><button class="btn btn-secondary btn-sm" data-action="mods" type="button" title="Mod files you already have: identify and copy to a drive (Ctrl+M)">${SV(16, 16, ICONS.mod)} Mod Menus</button><button class="btn btn-secondary btn-sm" data-action="homebrew" type="button" title="PC/console homebrew utilities (reference cards)">${SV(16, 16, ICONS.mod)} Homebrew Tools</button><button class="btn btn-secondary btn-sm" data-action="xbox" type="button" title="Send games, DLC and mods to your modded Xbox over WiFi / LAN (Ctrl+Shift+X)">${SV(16, 16, ICONS.xbox)} Send to Xbox</button>` : ""}
        ${N.IS_DESKTOP ? `<button class="btn btn-secondary btn-sm" data-action="dlc" type="button" title="Pick Xbox 360 DLC or title update files and put them into their game (Ctrl+D)">${icon("plus")} Install DLC</button>` : ""}
        ${N.IS_DESKTOP ? `<button class="btn btn-secondary btn-sm" data-action="god" type="button" title="Convert Xbox 360 ISOs to GOD or extracted folder (Ctrl+G)">${icon("disc")} ISO → GOD / XEX${N.settings.autoIsoMode !== "off" ? ` <span class="pill">AUTO ${N.settings.autoIsoMode === "xex" ? "XEX" : "GOD"}</span>` : ""}</button>` : ""}
        <button class="icon-btn" data-action="settings" type="button" aria-label="Settings" title="Settings (Ctrl+,)">${SV(18, 18, ICONS.gear)}</button>
      </div>
    </header>
    <main class="main">
      ${renderTips()}
      ${renderSummary()}
      ${renderDesk()}
      ${
        !jobs.length
          ? `<section class="empty">
              <h2>Drop an archive.</h2>
              <p>Choose a destination, then drop one archive or a whole set. Each one lands in its own folder. Nothing leaves this tab.</p>
              <button class="drop" data-action="pick" type="button">
                <div class="drop-inner">
                  <div class="drop-art">${DROP_ART}</div>
                  <div><strong>Drop files here</strong><span>or click to browse · multi-select is fine · Ctrl/⌘ O</span></div>
                  <div class="formats">${fileIcon("a.zip")}${fileIcon("a.rar")}${fileIcon("a.7z")}${N.IS_DESKTOP ? fileIcon("a.iso") : ""}</div>
                </div>
              </button>
              <button class="ghost" data-action="sample" type="button">Try a sample ZIP</button>
            </section>`
          : `${renderTabs()}
             ${active ? renderJob(active) : ""}`
      }
      ${renderHistory()}
    </main>
    <div class="foot">Files stay on this computer. Everything runs offline${N.IS_DESKTOP ? (nativeOk ? " · 7-Zip engine" : " · built-in engine") : ""}.</div>
    ${renderComposer()}
    ${renderGod()}
    ${renderSettings()}
    ${renderLibrary()}${renderFolderConfirm()}
    ${renderMods()}${renderHomebrew()}${renderXbox()}
    ${renderShortcuts()}
    ${renderPassword()}
    ${renderExportFmt()}
    ${renderDlcPick()}
  `;
  bind();
  updatePresence();
}

function renderTabs() {
  const ready = jobs.filter((j) => j.files.length).length;
  return `<div class="tabs">${jobs
    .map((j) => {
      const on = j.id === activeId ? " active" : "";
      return `<div class="tab${on}"><button data-open="${j.id}">${escapeHtml(j.name)}</button><button class="close" data-close="${j.id}" aria-label="Close">×</button></div>`;
    })
    .join("")}<button class="btn btn-secondary" data-action="pick">Add archive</button>${
      ready
        ? `<button class="btn btn-primary" data-action="save-all">${folderCapable ? "Extract all to folder" : "Download all as ZIP"} (${ready})</button>`
        : ""
    }${jobs.length ? `<button class="btn btn-secondary" data-action="clear-all">Remove all (${jobs.length})</button>` : ""}</div>`;
}

function renderNativeActions(job) {
  const busy = job.phase === "saving" || job.phase === "opening";
  const has = job.files.length > 0;
  if (job.phase === "saving" || job.phase === "queued") return `<button class="btn btn-secondary" data-action="cancel-one" type="button">${job.phase === "queued" ? "Remove from queue" : "Cancel"}</button>`;
  return `<button class="btn btn-primary" data-action="extract-default" ${has && !busy ? "" : "disabled"}>Extract</button>
    <button class="btn btn-secondary" data-action="extract-ask" ${has && !busy ? "" : "disabled"}>Extract to…</button>
    <button class="btn btn-secondary" data-action="extract-here" ${has && !busy ? "" : "disabled"}>Extract here</button>`;
}

function renderJob(job) {
  const visible = job.files.filter((f) => !isHidden(f.path));
  const tree = filterTree(buildTree(visible), query);
  const selected = visible.find((f) => f.path === selectedPath) || null;
  return `
    <section class="panel">
      <div class="panel-top">
        <div>
          <div class="title-row">${fileIcon(job.name)}<h2>${escapeHtml(job.name)}</h2></div>
          <p class="meta">${formatBytes(job.size)}${job.format ? " · " + job.format.toUpperCase() : ""} · ${job.files.length} files</p>
        </div>
        <div class="actions">
          ${job.native ? renderNativeActions(job) : `
          <button class="btn btn-primary" data-action="save-folder" ${job.files.length ? "" : "disabled"}>${folderCapable ? (destHandle ? "Write this archive" : "Extract to folder") : "Download extracted ZIP"}</button>
          <button class="btn btn-secondary" data-action="save-zip" ${job.files.length ? "" : "disabled"}>Download as ZIP</button>`}
        </div>
      </div>
      <div class="progress"><div data-job-bar="${escapeHtml(job.id)}" class="qbar-fill"></div></div>
      <div class="status-line">${job.progress > 0 && job.progress < 100 ? `<span class="spin">◌</span>` : ""} <span data-job-status="${escapeHtml(job.id)}">${escapeHtml(job.status)}</span></div>
      ${job.error ? `<div class="error-box danger">${escapeHtml(job.error)}</div>` : ""}
      ${job.warnings?.length ? `<div class="error-box">${job.warnings.map(w => escapeHtml(w)).join("<br>")}</div>` : ""}
    </section>
    ${
      job.files.length
        ? `<section class="workspace">
            <div class="card">
              <div class="toolbar"><input data-search placeholder="Search files" value="${escapeHtml(query)}" /></div>
              <div class="tree">${tree.length ? renderTree(tree) : `<div class="preview-empty">No files match.</div>`}</div>
            </div>
            <div class="card" id="preview">${renderPreview(selected)}</div>
          </section>`
        : ""
    }
  `;
}

function bind() {
  app.querySelectorAll("[data-action=pick]").forEach((el) => el.addEventListener("click", () => pickAny()));
  bindNative();
  bindComposer();
  bindGod();
  bindSettings();
  bindLibrary();
  bindFolderConfirm();
  bindMods();
  bindHomebrew();
  bindXbox();
  bindExportFmt();
  bindDlcPick();
  app.querySelector("[data-sum-retry]")?.addEventListener("click", () => retryFailedBatch());
  app.querySelector("[data-queue-pause]")?.addEventListener("click", () => { N.settings.queuePaused = !N.settings.queuePaused; if (!N.settings.queuePaused) pump(); render(); });
  app.querySelectorAll("[data-queue-up]").forEach((b) => b.addEventListener("click", () => moveWaiting(b.dataset.queueUp, "up")));
  app.querySelectorAll("[data-queue-down]").forEach((b) => b.addEventListener("click", () => moveWaiting(b.dataset.queueDown, "down")));
  app.querySelectorAll("[data-queue-front]").forEach((b) => b.addEventListener("click", () => moveWaiting(b.dataset.queueFront, "front")));
  app.querySelector("[data-action=xbox]")?.addEventListener("click", () => openXbox());
  app.querySelector("[data-setting=notify]")?.addEventListener("change", (e) => { N.settings.notify = e.target.checked; });
  app.querySelector("[data-action=mods]")?.addEventListener("click", () => openMods());
  app.querySelector("[data-action=homebrew]")?.addEventListener("click", () => openHomebrew());
  app.querySelector("[data-action=library]")?.addEventListener("click", () => openLibrary());
  app.querySelector("[data-tips-done]")?.addEventListener("click", () => { localStorage.setItem(TIPS_KEY, "1"); render(); });
  app.querySelector("[data-sum-close]")?.addEventListener("click", () => { summary = null; render(); });
  app.querySelector("[data-sum-open]")?.addEventListener("click", () => summary?.lastOutput && window.__TAURI__.opener?.revealItemInDir(summary.lastOutput).catch((e) => toast(String(e))));
  app.querySelector("[data-sc-close]")?.addEventListener("click", () => { shortcutsOpen = false; render(); });
  app.querySelector("[data-sc-overlay]")?.addEventListener("click", (e) => { if (e.target === e.currentTarget) { shortcutsOpen = false; render(); } });
  app.querySelector("[data-action=dlc]")?.addEventListener("click", () => pickDlc().catch((e) => toast(String(e))));
  app.querySelector("[data-action=clear-dlc]")?.addEventListener("click", () => { dlcManual = null; render(); });
  app.querySelectorAll("[data-setting=autoIsoMode]").forEach((el) => el.addEventListener("change", (e) => {
    N.settings.autoIsoMode = e.target.value;
    const m = N.settings.autoIsoMode;
    toast(m === "off" ? "Auto-convert ISOs is off." : m === "xex" ? "Auto-convert ISOs → extracted folder (XEX)." : "Auto-convert ISOs → GOD.");
    render();
  }));
  app.querySelectorAll("[data-auto-iso]").forEach((b) => b.addEventListener("click", () => {
    N.settings.autoIsoMode = b.dataset.autoIso;
    toast(N.settings.autoIsoMode === "off" ? "Auto-convert ISOs is off." : N.settings.autoIsoMode === "xex" ? "Auto-convert ISOs → extracted folder (XEX)." : "Auto-convert ISOs → GOD.");
    render();
  }));
  app.querySelector("[data-action=god]")?.addEventListener("click", () => openGod());
  app.querySelector("[data-action=create]")?.addEventListener("click", () => openComposer());
  app.querySelector("[data-action=sample]")?.addEventListener("click", () => addFiles([makeSample()]));
  app.querySelector("[data-action=save-folder]")?.addEventListener("click", () => activeJob() && saveToFolder(activeJob()));
  app.querySelector("[data-action=save-all]")?.addEventListener("click", () => (N.IS_DESKTOP ? extractAllNative() : saveAllToFolder()));
  app.querySelector("[data-action=clear-all]")?.addEventListener("click", () => clearAll());
  if (!N.IS_DESKTOP) app.querySelector("[data-action=choose-folder]")?.addEventListener("click", () => chooseFolder().catch((err) => {
    if (err?.name !== "AbortError") toast(err?.message || String(err));
  }));
  if (!N.IS_DESKTOP) app.querySelector("[data-action=cancel]")?.addEventListener("click", () => {
    jobs.forEach((j) => { if (j.phase === "opening" || j.phase === "saving") j.cancelled = true; });
    toast("Stopping after the current file.");
  });
  app.querySelector("[data-setting=collision]")?.addEventListener("change", (e) => {
    collision = e.target.value;
    idbSet("collision", collision).catch(() => {});
  });
  app.querySelector("[data-setting=autoWrite]")?.addEventListener("change", (e) => {
    autoWrite = e.target.checked;
    idbSet("autoWrite", autoWrite).catch(() => {});
  });
  app.querySelector("[data-setting=skipJunk]")?.addEventListener("change", (e) => {
    skipJunk = e.target.checked;
    idbSet("skipJunk", skipJunk).catch(() => {});
  });
  app.querySelector("[data-action=save-zip]")?.addEventListener("click", () => activeJob() && downloadAllAsZip(activeJob()));
  app.querySelectorAll("[data-open]").forEach((el) =>
    el.addEventListener("click", () => {
      activeId = el.getAttribute("data-open");
      selectedPath = null;
      query = "";
      render();
    })
  );
  app.querySelectorAll("[data-close]").forEach((el) =>
    el.addEventListener("click", (e) => {
      e.stopPropagation();
      const id = el.getAttribute("data-close");
      const closing = jobs.find((j) => j.id === id);
      if (closing?.native && (closing.phase === "saving" || closing.phase === "queued")) cancelJob(closing);
      jobs = jobs.filter((j) => j.id !== id);
      if (activeId === id) activeId = jobs[0]?.id || null;
      selectedPath = null;
      render();
    })
  );
  bindTree(app.querySelector(".tree"));
  const search = app.querySelector("[data-search]");
  if (search) {
    search.addEventListener("input", () => {
      query = search.value;
      const active = activeJob();
      if (!active) return;
      const tree = filterTree(buildTree(active.files.filter((f) => !isHidden(f.path))), query);
      const treeEl = app.querySelector(".tree");
      if (!treeEl) return;
      treeEl.innerHTML = tree.length ? renderTree(tree) : `<div class="preview-empty">No files match.</div>`;
      bindTree(treeEl);
    });
  }
  app.querySelector("[data-action=save-one]")?.addEventListener("click", () => {
    const active = activeJob();
    const selected = active?.files.find((f) => f.path === selectedPath);
    if (selected) downloadFile(selected);
  });
  const form = app.querySelector("[data-pw]");
  if (form && passwordPrompt) {
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      const value = document.getElementById("pw")?.value || "";
      const resolve = passwordPrompt.resolve;
      passwordPrompt = null;
      render();
      resolve(value);
    });
    app.querySelector("[data-pw-cancel]")?.addEventListener("click", () => {
      const resolve = passwordPrompt.resolve;
      passwordPrompt = null;
      render();
      resolve("");
    });
  }
}

function bindNative() {
  if (!N.IS_DESKTOP) return;
  app.querySelector("[data-action=choose-folder]")?.addEventListener("click", async () => {
    const p = await N.pickFolder();
    if (!p) return;
    settings().destPath = p;
    settings().destMode = "folder";
    render();
  });
  app.querySelector("[data-setting=destMode]")?.addEventListener("change", async (e) => {
    settings().destMode = e.target.value;
    if (e.target.value === "folder" && !settings().destPath) {
      const p = await N.pickFolder();
      if (p) settings().destPath = p;
      else settings().destMode = "beside";
    }
    render();
  });
  app.querySelector("[data-action=cancel]")?.addEventListener("click", () => {
    jobs.forEach((j) => { if (j.phase === "saving" || j.phase === "queued") cancelJob(j); });
    jobs.forEach(cancelJobGods);
    toast("Cancelling…");
  });
  app.querySelectorAll("[data-cancel-gods]").forEach((b) =>
    b.addEventListener("click", () => { const j = jobs.find((x) => x.id === b.dataset.cancelGods); if (j) cancelJobGods(j); })
  );
  app.querySelectorAll("[data-cancel-job]").forEach((b) =>
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      const j = jobs.find((x) => x.id === b.dataset.cancelJob);
      if (j) cancelJob(j);
    })
  );
  app.querySelector("[data-action=clear-done]")?.addEventListener("click", () => {
    jobs = jobs.filter((j) => !(j.phase === "saved" || j.phase === "skipped" || j.phase === "failed"));
    if (!jobs.some((j) => j.id === activeId)) activeId = jobs[0]?.id || null;
    selectedPath = null;
    render();
  });
  app.querySelector("[data-action=clear-history]")?.addEventListener("click", () => {
    localStorage.removeItem(HISTORY_KEY);
    render();
  });
  app.querySelectorAll("[data-reveal]").forEach((b) =>
    b.addEventListener("click", () => {
      const e = loadHistory().find((x) => String(x.at) === b.dataset.reveal);
      if (e?.output) window.__TAURI__.opener?.revealItemInDir(e.output).catch((err) => toast(String(err)));
    })
  );
  const job = activeJob();
  if (!job?.native) return;
  app.querySelector("[data-action=extract-default]")?.addEventListener("click", () => enqueue(job, {}));
  app.querySelector("[data-action=extract-ask]")?.addEventListener("click", () => enqueue(job, { mode: "ask" }));
  app.querySelector("[data-action=extract-here]")?.addEventListener("click", () => enqueue(job, { layout: "here", mode: "beside" }));
  app.querySelector("[data-action=cancel-one]")?.addEventListener("click", () => cancelJob(job));
}

async function extractAllNative() {
  const ready = jobs.filter((j) => j.native && j.files.length && j.phase === "ready");
  if (!ready.length) return toast("Nothing is ready to extract yet.");
  let dest = null;
  if (settings().destMode === "ask") {
    dest = await N.pickFolder("Extract all into…");
    if (!dest) return;
  } else if (settings().destMode === "folder" && settings().destPath) dest = settings().destPath;
  if (dest && !(await preflightDest(dest, ready.map((j) => j.path), N.settings.autoIsoMode))) return;
  for (const job of ready) await enqueue(job, dest ? { dest } : {});
}

async function pickAny() {
  if (!N.IS_DESKTOP) return fileInput.click();
  try {
    const paths = await N.pickArchives();
    if (paths.length) await addPaths(paths);
  } catch (err) {
    toast(String(err));
  }
}

function bindTree(treeEl) {
  if (!treeEl) return;
  treeEl.querySelectorAll("[data-file]").forEach((el) =>
    el.addEventListener("click", () => {
      selectedPath = decodeURIComponent(el.getAttribute("data-file"));
      const active = activeJob();
      const selected = active?.files.find((f) => f.path === selectedPath) || null;
      const pane = document.getElementById("preview");
      if (pane) {
        pane.innerHTML = renderPreview(selected);
        pane.querySelector("[data-action=save-one]")?.addEventListener("click", () => selected && downloadFile(selected));
      }
      treeEl.querySelectorAll(".row").forEach((r) => r.classList.remove("selected"));
      el.classList.add("selected");
    })
  );
}

fileInput.addEventListener("change", async () => {
  const picked = fileInput.files ? [...fileInput.files] : [];
  if (picked.length) await addFiles(picked);
});
window.addEventListener("keydown", (e) => {
  const mod = e.metaKey || e.ctrlKey;
  const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName || "");
  const k = e.key.toLowerCase();
  if (mod && k === "o") { e.preventDefault(); pickAny(); return; }
  if (!N.IS_DESKTOP) return;
  if (mod && k === ",") { e.preventDefault(); settingsOpen = true; render(); }
  else if (mod && k === "l") { e.preventDefault(); openLibrary(); }
  else if (mod && k === "m") { e.preventDefault(); openMods(); }
  else if (mod && k === "d") { e.preventDefault(); pickDlc().catch((err) => toast(String(err))); }
  else if (mod && k === "g") { e.preventDefault(); openGod(); }
  else if (mod && e.shiftKey && k === "x" && N.IS_DESKTOP) { e.preventDefault(); openXbox(); }
  else if (mod && k === "n" && nativeOk) { e.preventDefault(); openComposer(); }
  else if (!mod && e.key === "?" && !typing) { shortcutsOpen = true; render(); }
  else if (e.key === "Escape") {
    if (shortcutsOpen) { shortcutsOpen = false; render(); }
    else if (folderConfirm) { folderConfirm = null; render(); }
    else if (modsView) { modsView = null; render(); }
    else if (xboxView && !xboxView.sends.some((s) => s.running)) { xboxView = null; render(); }
    else if (libraryView) closeLibrary();
    else if (settingsOpen) { settingsOpen = false; confirmDelIso = false; foundRoots = null; render(); }
  }
});
["dragenter", "dragover", "dragleave", "drop"].forEach((evt) =>
  window.addEventListener(evt, (e) => {
    e.preventDefault();
    if (evt === "dragenter") {
      dragDepth += 1;
      document.querySelector(".drop")?.classList.add("dragging");
    }
    if (evt === "dragleave") {
      dragDepth = Math.max(0, dragDepth - 1);
      if (!dragDepth) document.querySelector(".drop")?.classList.remove("dragging");
    }
    if (evt === "drop") {
      dragDepth = 0;
      document.querySelector(".drop")?.classList.remove("dragging");
      if (e.dataTransfer?.files?.length) addFiles(e.dataTransfer.files);
    }
  })
);

async function initDesktop() {
  if (!N.IS_DESKTOP) return;
  try {
    nativeOk = (await N.invoke("native_info")).available;
  } catch {
    nativeOk = false;
  }
  await N.initNativeEvents();
  await N.invoke("discord_set_enabled", { enabled: N.settings.discordPresence }).catch(() => {});
  // One-time notice when the update had to set the older settings profile aside (R19 L5).
  for (const msg of await N.invoke("migration_notices").catch(() => [])) showMigrationNotice(msg);
  // R22 L(a): repair a save restore that was cut off (crash / power loss) last time. Only
  // consoles with a local restore record are contacted; an unreachable one is retried later.
  N.invoke("saves_recover", { consoles: (N.settings.consoles || []).map((k) => ({ host: k.host, port: +k.port || 21, user: k.user || "", pass: k.pass || "" })) })
    .then((msgs) => { for (const msg of msgs || []) showMigrationNotice(msg); })
    .catch(() => {});
  // Archives passed on the command line, by double-click, or from the Explorer menu.
  // A second launch while we're open forwards its request here (single-instance).
  let draining = Promise.resolve();
  const drainLaunch = () => {
    draining = draining.then(async () => {
      const reqs = await N.invoke("take_launch_requests").catch(() => []);
      for (const r of reqs) await addPaths(r.paths, r.action);
    });
    return draining;
  };
  await window.__TAURI__.event.listen("launch-request", () => drainLaunch());
  await N.onNativeDrop(
    (paths) => addPaths(paths),
    (on) => document.querySelector(".drop")?.classList.toggle("dragging", on)
  );
  render();
  drainLaunch();
  if (N.settings.releaseCheck) {
    try {
      const current = await N.appVersion();
      if (!current) throw new Error("no app version");
      const info = await N.invoke("check_release", { current, optIn: true });
      releaseInfo = info;
      if (info?.available) toast(`Update available: ${info.latest}`);
    } catch {}
  }
}

restoreDesk().then(initDesktop);

// Test bridge (examples/bridge.rs on 127.0.0.1, any UNPACK_BRIDGE_PORT, with tests/test-shim.js
// loaded): expose queue state for UI tests. The real app is never served over http://127.0.0.1.
if (typeof window !== "undefined" && location.protocol === "http:" && location.hostname === "127.0.0.1" && window.__TEST) {
  window.__unpackQueue = () => ({ waiting: waiting.length, phases: jobs.map((j) => j.phase), paused: N.settings.queuePaused });
  window.showError = showError;
  window.__unpackRepaint = () => { for (const j of jobs) { paintJob(j); for (const g of j.gods || []) paintJob(g); } };
  window.__unpackTestPause = () => {
    const job = { id: uid(), native: true, path: "/tmp/fake.7z", name: "fake.7z", size: 1, files: [{ path: "a", size: 1 }], warnings: [], format: "7z", phase: "queued", status: "Waiting in queue…", progress: 0, error: "", password: "", cancelled: false };
    jobs.push(job);
    waiting.push({ job, opts: { dest: "/tmp/out" } });
    render();
  };
}
