import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const root = new URL("../src", import.meta.url).pathname;
const files = [];
const walk = (dir) => {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p);
    else if (/\.(js|html)$/i.test(name)) files.push(p);
  }
};
walk(root);

let fails = 0;
for (const f of files) {
  const text = readFileSync(f, "utf8");
  if (/style\s*=/.test(text)) {
    console.error(`FAIL inline style= in ${f}`);
    fails++;
  }
  if (/setAttribute\s*\(\s*['"]style['"]/.test(text)) {
    console.error(`FAIL setAttribute('style' in ${f}`);
    fails++;
  }
  if (/\.style\.cssText\s*=/.test(text)) {
    console.error(`FAIL .style.cssText assignment in ${f}`);
    fails++;
  }
}
if (fails) {
  console.error(`${fails} file(s) with forbidden inline style usage`);
  process.exit(1);
}
console.log("PASS no inline style= in src HTML strings");
