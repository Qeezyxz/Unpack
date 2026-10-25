// R21 L1/L2: settings import keeps saved passwords and checks values.
import { planImport, isSystemPath, isUserPath } from "../src/importcheck.js";
let fails = 0;
const check = (n, ok, info = "") => { if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + n + (info ? " — " + info : "")); };
const cur = { consoles: JSON.stringify([{ name: "X", host: "192.168.1.5", port: 21, user: "xboxftp", pass: "MySecret" }]) };
const r = planImport({
  consoles: JSON.stringify([{ name: "X", host: "192.168.1.5", port: 21, user: "xboxftp", pass: "" }, { name: "Y", host: "192.168.1.6", port: 21, user: "xboxftp", pass: "" }]),
  releaseCheck: "1", coverNet: "1", discord: "1",
  destPath: "C:\\Windows\\System32", libraries: JSON.stringify(["C:\\Games"]),
  mods: JSON.stringify([{ path: "C:\\Windows\\System32\\x.dll" }, { path: "D:\\Mods\\menu.xex" }]),
  maxJobs: "99", dlMaxGb: "50", theme: "<img onerror=x>", notify: "0", autoTu: "yes", destMode: "folder",
  libQuery: "halo", unknownKey: "1",
}, (k) => cur[k] ?? null);
const cons = JSON.parse(r.apply.consoles);
check("L1: blank imported pass keeps the saved one", cons[0].pass === "MySecret", JSON.stringify(cons));
check("L1: a new console stays blank", cons[1].pass === "");
check("L2: network opt-ins are not applied", !("releaseCheck" in r.apply) && !("coverNet" in r.apply) && !("discord" in r.apply));
check("L2: opt-ins are returned for confirmation", ["releaseCheck", "coverNet", "discord"].every((k) => r.optIns.includes(k)), r.optIns.join());
check("L2: destPath/libraries never imported", !("destPath" in r.apply) && !("libraries" in r.apply));
check("L2: system-folder mod paths dropped", JSON.parse(r.apply.mods).length === 1 && JSON.parse(r.apply.mods)[0].path === "D:\\Mods\\menu.xex");
check("L2: out-of-range / bad values dropped", !("maxJobs" in r.apply) && !("theme" in r.apply) && !("autoTu" in r.apply) && !("destMode" in r.apply));
check("L2: good values kept", r.apply.dlMaxGb === "50" && r.apply.notify === "0" && r.apply.libQuery === "halo");
check("L2: unknown keys ignored", !("unknownKey" in r.apply));
check("L2: opt-in off is fine", planImport({ coverNet: "0" }, () => null).apply.coverNet === "0");
check("isSystemPath", isSystemPath("C:\\Windows") && isSystemPath("c:/program files (x86)/x") && isSystemPath("D:\\x\\System32") && isSystemPath("D:\\Windows\\System32") && !isSystemPath("C:\\Users\\me\\Games") && isSystemPath("C:\\Users\\me\\AppData\\Local") && !isSystemPath("E:\\Windows Games\\x") && !isSystemPath("D:\\ProgramData2\\x") && !isSystemPath("C:\\Users\\me\\Games\\AppData") && isSystemPath("  C:\\Windows"));
// R22 M2: network shares, device paths, "..", 8.3 short names and trailing space/dot are refused.
for (const bad of ["\\\\server\\share\\m.xex", "//server/share/m.xex", "\\\\?\\C:\\m.xex", "\\\\.\\C:\\m.xex",
  "C:\\Mods\\..\\Windows\\m.xex", "/home/me/../../etc/x", "C:\\PROGRA~1\\m.xex", "C:\\Mods \\m.xex", "C:\\Mods.\\m.xex", "D:\\Mods\\m.xex."])
  check("M2: refused " + bad, !isUserPath(bad));
check("M2: plain paths still fine", isUserPath("D:\\Mods\\menu.xex") && isUserPath("/home/me/mods/a.xex") && isUserPath("D:\\My..Mods\\a.b.xex"));
check("M2: UNC mods dropped on import", JSON.parse(planImport({ mods: JSON.stringify([{ path: "\\\\evil\\s\\m.xex" }]) }, () => null).apply.mods).length === 0);
// R22 L(d): __proto__ / constructor / prototype keys are ignored, never crash or pollute.
{
  let pr = null, err = "";
  try { pr = planImport(JSON.parse('{"constructor":"x","__proto__":{"polluted":"1"},"prototype":"z","toString":"q","theme":"dark"}'), () => null); } catch (e) { err = String(e); }
  check("L(d): no crash on prototype keys", pr !== null, err);
  check("L(d): only real settings applied", pr && Object.keys(pr.apply).join() === "theme" && pr.dropped.length === 0, pr && JSON.stringify(pr));
  check("L(d): nothing polluted", ({}).polluted === undefined && (pr ? pr.apply.polluted === undefined : true));
}
console.log(fails ? `${fails} FAILED` : "all passed");
process.exit(fails ? 1 : 0);
