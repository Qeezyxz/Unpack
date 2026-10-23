import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("./theme.js", import.meta.url), "utf8");

function run(prefersLight, stored) {
  const listeners = [];
  const documentElement = { attrs: {}, setAttribute(k, v) { this.attrs[k] = v; } };
  const context = {
    window: {},
    document: { documentElement },
    localStorage: {
      getItem: () => stored,
    },
    matchMedia: () => ({
      matches: prefersLight,
      addEventListener: (_e, fn) => listeners.push(fn),
    }),
  };
  context.window = context;
  vm.createContext(context);
  vm.runInContext(source, context);
  return documentElement.attrs["data-theme"];
}

const cases = [
  [true, "system", "light"],
  [false, "system", "dark"],
  [true, "dark", "dark"],
  [false, "light", "light"],
];
for (const [prefers, stored, expect] of cases) {
  const got = run(prefers, stored);
  if (got !== expect) {
    console.error(`FAIL prefers=${prefers} stored=${stored} got=${got} expected=${expect}`);
    process.exit(1);
  }
}
console.log("theme.js: 4/4 PASS");
