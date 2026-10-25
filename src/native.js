// Native (desktop) layer for Unpack. Talks to the Rust side, which drives the bundled 7-Zip.
// Loaded by app.js; everything here is a no-op in a plain browser.

const T = window.__TAURI__;
export const IS_DESKTOP = !!(T && T.core);
export const invoke = (cmd, args) => T.core.invoke(cmd, args);
/** The app's own version from Tauri (tauri.conf.json), or null when it can't be read. */
export async function appVersion() {
  try { return T?.app?.getVersion ? String(await T.app.getVersion()) : null; } catch { return null; }
}

const LS = "unpack.";
export const settings = {
  get destMode() { return localStorage.getItem(LS + "destMode") || "beside"; }, // beside | folder | ask
  set destMode(v) { localStorage.setItem(LS + "destMode", v); },
  get destPath() { return localStorage.getItem(LS + "destPath") || ""; },
  set destPath(v) { localStorage.setItem(LS + "destPath", v || ""); },
  // Off | god | xex — ISOs found inside extracted archives.
  get autoIsoMode() {
    const v = localStorage.getItem(LS + "autoIso") || "";
    if (v === "god" || v === "xex") return v;
    // migrate legacy autoGod checkbox
    if (localStorage.getItem(LS + "autoGod") === "1") return "god";
    return "off";
  },
  set autoIsoMode(v) {
    const m = v === "god" || v === "xex" ? v : "off";
    localStorage.setItem(LS + "autoIso", m);
    localStorage.setItem(LS + "autoGod", m === "god" ? "1" : "0");
  },
  get autoGod() { return this.autoIsoMode === "god"; },
  set autoGod(v) { if (v) this.autoIsoMode = "god"; else if (this.autoIsoMode === "god") this.autoIsoMode = "off"; },
  get deleteIsoAfterGod() { return localStorage.getItem(LS + "delIso") === "1"; }, // off by default
  set deleteIsoAfterGod(v) { localStorage.setItem(LS + "delIso", v ? "1" : "0"); },
  // DLC: auto-install after extract (copy, non-destructive) is on by default; move is off.
  get autoDlc() { return localStorage.getItem(LS + "autoDlc") !== "0"; },
  set autoDlc(v) { localStorage.setItem(LS + "autoDlc", v ? "1" : "0"); },
  get autoTu() { return localStorage.getItem(LS + "autoTu") !== "0"; },
  set autoTu(v) { localStorage.setItem(LS + "autoTu", v ? "1" : "0"); },
  get moveDlc() { return localStorage.getItem(LS + "moveDlc") === "1"; },
  set moveDlc(v) { localStorage.setItem(LS + "moveDlc", v ? "1" : "0"); },
  get libraries() { try { const v = JSON.parse(localStorage.getItem(LS + "libraries") || "[]"); return Array.isArray(v) ? v.filter((x) => typeof x === "string") : []; } catch { return []; } },
  set libraries(v) { localStorage.setItem(LS + "libraries", JSON.stringify([...new Set(v)])); },
  // Mod Menus library: [{path, notes, mode}] - paths of files the user added, nothing else.
  get mods() { try { const v = JSON.parse(localStorage.getItem(LS + "mods") || "[]"); return Array.isArray(v) ? v.filter((x) => x && typeof x.path === "string").map((x) => ({ path: x.path, notes: String(x.notes || "").slice(0, 500), mode: ["plugins", "content", "as_is"].includes(x.mode) ? x.mode : "", tid: /^[0-9A-F]{8}$/.test(x.tid || "") ? x.tid : "", gname: String(x.gname || "").slice(0, 120) })) : []; } catch { return []; } },
  set mods(v) { localStorage.setItem(LS + "mods", JSON.stringify(v)); },
  // Known mod names the user taught Unpack: [{key, tid, gname}]. Starts empty; nothing bundled.
  get modNames() { try { const v = JSON.parse(localStorage.getItem(LS + "modNames") || "[]"); const seen = new Set(); return Array.isArray(v) ? v.filter((x) => x && typeof x.key === "string" && x.key.trim() && /^[0-9A-F]{8}$/.test(x.tid || "")).map((x) => ({ key: x.key.slice(0, 80), tid: x.tid, gname: String(x.gname || "").slice(0, 120) })).filter((x) => !seen.has(x.key) && seen.add(x.key)).slice(0, 500) : []; } catch { return []; } },
  set modNames(v) { localStorage.setItem(LS + "modNames", JSON.stringify(v)); },
  get theme() { return localStorage.getItem(LS + "theme") || "system"; }, // system | light | dark
  set theme(v) { localStorage.setItem(LS + "theme", v); },
  get notify() { return localStorage.getItem(LS + "notify") !== "0"; }, // on by default
  set notify(v) { localStorage.setItem(LS + "notify", v ? "1" : "0"); },
  // Saved download links: [{id,name,url,notes}] — user-entered only, never bundled.
  get savedLinks() { try { const v = JSON.parse(localStorage.getItem(LS + "links") || "[]"); return Array.isArray(v) ? v.filter((x) => x && typeof x.url === "string").map((x) => ({ id: x.id || "", name: String(x.name || "").slice(0, 120), url: x.url, notes: String(x.notes || "").slice(0, 500), game: String(x.game || "").slice(0, 120), sha: String(x.sha || "").slice(0, 64) })).slice(0, 200) : []; } catch { return []; } },
  set savedLinks(v) { localStorage.setItem(LS + "links", JSON.stringify(v)); },
  // Send to Xbox: saved consoles [{name, host, port, user, pass}] (LAN IPs only; Rust re-checks).
  get consoles() { try { const v = JSON.parse(localStorage.getItem(LS + "consoles") || "[]"); return Array.isArray(v) ? v.filter((x) => x && typeof x.host === "string" && /^\d{1,3}(\.\d{1,3}){3}$/.test(x.host.trim())).map((x) => ({ name: String(x.name || "My Xbox").slice(0, 60), host: x.host.trim(), port: Number.isInteger(+x.port) && +x.port > 0 && +x.port < 65536 ? +x.port : 21, user: String(x.user || "xboxftp").slice(0, 64), pass: String(x.pass ?? "xboxftp").slice(0, 128), parallel: x.parallel === true })).slice(0, 10) : []; } catch { return []; } },
  // Last custom destination per Send to Xbox type: {god|dlc|tu|xex|plugins|custom: "/Hdd1/..."}
  get xboxLastDest() { try { const v = JSON.parse(localStorage.getItem(LS + "xboxLastDest") || "{}"); const o = {}; for (const k of ["god", "dlc", "tu", "xex", "plugins", "custom"]) if (typeof v[k] === "string" && /^\/[A-Za-z][A-Za-z0-9]{0,15}(\/[^\/]{1,42})*$/.test(v[k]) && v[k].length < 600) o[k] = v[k]; return o; } catch { return {}; } },
  set xboxLastDest(v) { localStorage.setItem(LS + "xboxLastDest", JSON.stringify(v)); },
  // Chosen console drive per console {"host:port": "Usb1"} (drive names as the console lists them).
  get xboxDrives() { try { const v = JSON.parse(localStorage.getItem(LS + "xboxDrives") || "{}"); const o = {}; for (const [k, d] of Object.entries(v || {}).slice(0, 20)) if (/^\d{1,3}(\.\d{1,3}){3}:\d{1,5}$/.test(k) && +k.split(":")[1] >= 1 && +k.split(":")[1] <= 65535 && k.split(":")[0].split(".").every((x) => +x <= 255) && typeof d === "string" && /^[A-Za-z][A-Za-z0-9]{0,15}$/.test(d)) o[`${k.split(":")[0].split(".").map(Number).join(".")}:${+k.split(":")[1]}`] = d; return o; } catch { return {}; } },
  set xboxDrives(v) { localStorage.setItem(LS + "xboxDrives", JSON.stringify(v)); },
  set consoles(v) { localStorage.setItem(LS + "consoles", JSON.stringify(v)); },
  // Jobs at once (extract queue). 1-4, default 2.
  get maxJobs() { const n = Math.round(Number(localStorage.getItem(LS + "maxJobs"))); return n >= 1 && n <= 4 ? n : 2; },
  set maxJobs(v) { localStorage.setItem(LS + "maxJobs", String(Math.min(4, Math.max(1, Math.round(Number(v)) || 2)))); },
  // "Open zips inside zips" (extract archives found inside an archive too). On by default.
  get nested() { return localStorage.getItem(LS + "nested") !== "0"; },
  set nested(v) { localStorage.setItem(LS + "nested", v ? "1" : "0"); },
  // Discord Rich Presence (local IPC to the Discord app). Off by default; turning it off clears it.
  get discordPresence() { return localStorage.getItem(LS + "discord") === "1"; }, // off unless turned on
  set discordPresence(v) { localStorage.setItem(LS + "discord", v ? "1" : "0"); },
  get dlMaxGb() { const n = Number(localStorage.getItem(LS + "dlMaxGb")); return Number.isFinite(n) && n > 0 ? n : 20; },
  set dlMaxGb(v) { localStorage.setItem(LS + "dlMaxGb", String(Math.max(1, Math.min(500, Number(v) || 20)))); },
  /** ask | always | never — auto-queue other discs of the same game when converting ISOs. */
  get companionDiscMode() {
    const v = localStorage.getItem(LS + "companionDisc") || "ask";
    return v === "always" || v === "never" ? v : "ask";
  },
  set companionDiscMode(v) { localStorage.setItem(LS + "companionDisc", v === "always" || v === "never" ? v : "ask"); },
  get releaseCheck() { return localStorage.getItem(LS + "releaseCheck") === "1"; },
  set releaseCheck(v) { localStorage.setItem(LS + "releaseCheck", v ? "1" : "0"); },
  get coverArtNetwork() { return localStorage.getItem(LS + "coverNet") === "1"; },
  set coverArtNetwork(v) { localStorage.setItem(LS + "coverNet", v ? "1" : "0"); },
  get destPresets() {
    try {
      const v = JSON.parse(localStorage.getItem(LS + "destPresets") || "[]");
      return Array.isArray(v)
        ? v.filter((x) => x && typeof x.name === "string" && typeof x.path === "string" && /^\/[A-Za-z][A-Za-z0-9]{0,15}(\/[^\/]{1,42})*$/.test(x.path) && x.path.length < 600)
            .map((x) => ({ name: String(x.name).slice(0, 40), path: x.path }))
            .slice(0, 20)
        : [];
    } catch { return []; }
  },
  set destPresets(v) { localStorage.setItem(LS + "destPresets", JSON.stringify(v)); },
  get queuePaused() { return localStorage.getItem(LS + "queuePaused") === "1"; },
  set queuePaused(v) { localStorage.setItem(LS + "queuePaused", v ? "1" : "0"); },
  get libSort() { return localStorage.getItem(LS + "libSort") || "name"; },
  set libSort(v) { localStorage.setItem(LS + "libSort", v || "name"); },
  get libFilterKind() { return localStorage.getItem(LS + "libFilterKind") || "all"; },
  set libFilterKind(v) { localStorage.setItem(LS + "libFilterKind", v || "all"); },
  get libQuery() { return localStorage.getItem(LS + "libQuery") || ""; },
  set libQuery(v) { localStorage.setItem(LS + "libQuery", String(v || "").slice(0, 120)); },
};

export const ARCHIVE_RE = /\.(zip|rar|7z|tar|tgz|gz|bz2|xz|tar\.gz|tar\.bz2|tar\.xz|cab|arj|lzh|z|lzma|wim|zipx)$/i;
export const baseName = (p) => String(p).split(/[\\/]/).filter(Boolean).pop() || String(p);
export const dirName = (p) => {
  const s = String(p).replace(/[\\/]+$/, "");
  const i = Math.max(s.lastIndexOf("/"), s.lastIndexOf("\\"));
  if (i < 0) return s;
  const d = s.slice(0, i);
  return /^[A-Za-z]:$/.test(d) ? d + "\\" : d || "/";
};

export async function pickArchives() {
  return await pickFiles("Open archives");
}

// Pickers run in Rust so the picked paths are "granted": copy/install commands only act on
// paths the user picked, or that came from a scan or an extraction.
export async function pickFolder(title = "Choose a destination folder") {
  const res = await invoke("pick_path", { kind: "folder", title });
  return res[0] || null;
}
export async function pickFiles(title) {
  return await invoke("pick_path", { kind: "files", title });
}

export async function pickSave(defaultPath, filters, title = "Save file") {
  // Rust picker so the parent folder is granted for create_archive.
  return await invoke("pick_save", { title, defaultPath: defaultPath || null, filters: filters || null });
}

/** Write a text export to a path returned by pickSave (the Rust side re-checks the grant). */
export async function saveText(path, text) {
  return await invoke("save_text_file", { path, text });
}

/** Subscribe to native drag & drop; calls onDrop(paths) and onHover(bool). */
export async function onNativeDrop(onDrop, onHover) {
  const wv = T.webview?.getCurrentWebview?.();
  if (!wv?.onDragDropEvent) return;
  await wv.onDragDropEvent((ev) => {
    const t = ev.payload?.type;
    if (t === "enter" || t === "over") onHover(true);
    else if (t === "leave") onHover(false);
    else if (t === "drop") {
      onHover(false);
      const paths = ev.payload.paths || [];
      if (!paths.length) return;
      void (async () => {
        const allowed = await invoke("session_drop_prepare", { paths });
        if (allowed?.length) onDrop(allowed);
      })();
    }
  });
}

/** job-progress events from Rust, routed by job id. */
const progressHandlers = new Map();
export function onJobProgress(id, fn) {
  progressHandlers.set(id, fn);
  return () => progressHandlers.delete(id);
}
const libraryScanHandlers = new Map();
export function onLibraryScanProgress(id, fn) {
  libraryScanHandlers.set(id, fn);
  return () => libraryScanHandlers.delete(id);
}
export async function initNativeEvents(opts = {}) {
  await T.event.listen("job-progress", (e) => {
    const p = e.payload;
    progressHandlers.get(p.id)?.(p);
  });
  await T.event.listen("library-scan-progress", (e) => {
    const p = e.payload;
    libraryScanHandlers.get(p.id)?.(p);
  });
  await T.event.listen("path-grant-refused", (e) => {
    const msg = typeof e.payload === "string" ? e.payload : e.payload?.message;
    if (msg) opts.onPathGrantRefused?.(msg);
  });
}

export function errCode(err) {
  return String(err?.message || err || "");
}
