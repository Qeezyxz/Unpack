// Settings import checks (R21 L1/L2). Pure: no DOM, no storage, so tests can run it in node.
// planImport(raw, current) -> { apply: {key: string}, optIns: [key], dropped: [key] }
//   raw:     the parsed "settings" object from an export file
//   current: (key) => the value stored now (string or null)
// Network opt-ins are never in `apply`; they come back in `optIns` for the caller to confirm.

const FLAG = ["delIso", "autoDlc", "autoTu", "moveDlc", "notify", "nested"];
const OPT_IN = ["releaseCheck", "coverNet", "discord"];
const ENUM = Object.assign(Object.create(null), {
  destMode: ["beside", "ask"], // "folder" needs a folder picked on this PC, so it isn't imported
  autoIso: ["off", "god", "xex"],
  theme: ["system", "light", "dark"],
  companionDisc: ["ask", "always", "never"],
  libSort: ["name", "title", "size", "dlc", "tu"],
  libFilterKind: ["all", "god", "xex", "dlc", "tu"],
});
const hasOwn = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
const BAD_KEYS = new Set(["__proto__", "constructor", "prototype"]);
const JSON_ARRAY = ["mods", "modNames", "links", "consoles", "destPresets"];
const JSON_OBJECT = ["xboxLastDest", "xboxDrives"];
// Local PC folders are never taken from a file: pick them again on this PC.
const LOCAL_PATHS = ["destPath", "libraries"];

/** A Windows or Unix system folder (or anything inside one). */
export function isSystemPath(p) {
  const s = String(p || "").replace(/^\\\\\?\\/, "").replace(/\//g, "\\").toLowerCase();
  if (/^[a-z]:\\(windows|program files|program files \(x86\)|programdata|\$recycle\.bin|system volume information)(\\|$)/.test(s)) return true;
  if (/\\(system32|syswow64)(\\|$)/.test(s)) return true;
  if (/^\\(etc|usr|bin|sbin|boot|dev|proc|sys|system|library|windows)(\\|$)/.test(s)) return true;
  return false;
}
/** Looks like an absolute user path on this PC: C:\... or /home/... and not a system folder.
 *  R22 M2: no network shares (\\ or //), device paths (\\?\ \\.\), ".." segments, 8.3
 *  short names (PROGRA~1) or segments ending in a space or dot. */
export function isUserPath(p) {
  if (typeof p !== "string" || p.length >= 1000 || /[\u0000-\u001f]/.test(p)) return false;
  if (/^[\\/]{2}/.test(p)) return false;
  if (!(/^[A-Za-z]:[\\/]/.test(p) || /^\//.test(p))) return false;
  const segs = p.replace(/^[A-Za-z]:/, "").split(/[\\/]/).filter((x) => x !== "");
  for (const g of segs) {
    if (g === ".." || g === "." || /~\d/.test(g) || /[ .]$/.test(g)) return false;
  }
  return !isSystemPath(p);
}

const str = (v) => (typeof v === "string" ? v : v == null ? null : JSON.stringify(v));
function parse(v) { try { return JSON.parse(v); } catch { return undefined; } }

export function planImport(raw, current) {
  // R22 L(d): null-prototype result, and prototype-ish keys are never looked at.
  const apply = Object.create(null), optIns = [], dropped = [];
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return { apply, optIns, dropped };
  for (const [k0, v0] of Object.entries(raw)) {
    const k = k0.startsWith("unpack.") ? k0.slice(7) : k0;
    if (BAD_KEYS.has(k) || BAD_KEYS.has(k0)) continue;
    const v = str(v0);
    let ok = null;
    if (v == null) ok = null;
    else if (FLAG.includes(k)) ok = v === "0" || v === "1" ? v : null;
    else if (OPT_IN.includes(k)) {
      if (v === "1") { optIns.push(k); continue; }
      ok = v === "0" ? v : null;
    } else if (hasOwn(ENUM, k)) ok = ENUM[k].includes(v) ? v : null;
    else if (k === "maxJobs") ok = /^[1-4]$/.test(v) ? v : null;
    else if (k === "dlMaxGb") ok = /^\d{1,3}$/.test(v) && +v >= 1 && +v <= 500 ? String(+v) : null;
    else if (k === "libQuery") ok = v.length <= 200 && !/[\u0000-\u001f]/.test(v) ? v : null;
    else if (LOCAL_PATHS.includes(k)) ok = null;
    else if (JSON_ARRAY.includes(k)) {
      let a = parse(v);
      if (!Array.isArray(a) || a.length > 1000) ok = null;
      else {
        if (k === "mods") a = a.filter((m) => m && isUserPath(m.path));
        if (k === "consoles") {
          // L1: an export made without passwords has pass "": keep the password already saved
          // for the same console (host + port + user).
          const have = parse(current("consoles") || "[]");
          const old = Array.isArray(have) ? have : [];
          const id = (c) => `${String(c?.host || "").trim()}:${+c?.port || 21}:${c?.user || "xboxftp"}`;
          a = a.filter((c) => c && typeof c === "object").map((c) => {
            if (c.pass) return c;
            const m = old.find((o) => o && id(o) === id(c) && o.pass);
            return m ? { ...c, pass: m.pass } : c;
          });
        }
        ok = JSON.stringify(a);
      }
    } else if (JSON_OBJECT.includes(k)) {
      const o = parse(v);
      ok = o && typeof o === "object" && !Array.isArray(o) ? v : null;
    } else continue; // not a setting we export: ignore silently
    if (ok == null) dropped.push(k); else apply[k] = ok;
  }
  return { apply, optIns, dropped };
}
