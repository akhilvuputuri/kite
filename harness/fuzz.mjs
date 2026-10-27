#!/usr/bin/env node
// Differential tests against the npm that ships with Node:
//   node-semver satisfies() with loose parsing, and
//   localeCompare(b, 'en') ordering.
// Usage: fuzz.mjs <lockstep-binary> [cases]
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const [bin, countArg] = process.argv.slice(2);
const count = Number(countArg || 20000);
const npmRoot = dirname(dirname(execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim() + "/x"));
const require = createRequire(join(npmRoot, "node_modules", "npm", "package.json"));
const semver = require("semver");

let seed = 42;
const rand = (n) => {
  seed = (seed * 1103515245 + 12345) % 2147483648;
  return seed % n;
};
const pick = (xs) => xs[rand(xs.length)];
const num = () => pick(["0", "1", "2", "3", "10", "x", "*", "X"]);
const pre = () => pick(["", "", "", "-0", "-alpha", "-alpha.1", "-beta.2", "-rc.10", "-1"]);
const version = () => `${pick(["0", "1", "2", "3"])}.${pick(["0", "1", "2", "10"])}.${pick(["0", "1", "3", "9"])}${pre()}`;
const partial = () => {
  const n = rand(3);
  return [num(), num(), num()].slice(0, n + 1).join(".") + (n === 2 ? pre() : "");
};
const comparator = () => pick(["", "^", "~", ">=", "<=", ">", "<", "=", "~>", "v", ">= "]) + partial();
const set = () => {
  if (rand(6) === 0) return `${partial()} - ${partial()}`;
  return Array.from({ length: 1 + rand(2) }, comparator).join(" ");
};
const range = () => Array.from({ length: 1 + rand(2) }, set).join(" || ");

const cases = [];
for (let i = 0; i < count; i++) cases.push([rand(10) === 0 ? pick(["", "*", "latest", "1.2.3.4", "^1.2.3 junk"]) : range(), version()]);
const expected = cases.map(([r, v]) => {
  if (semver.validRange(r, true) === null) return "invalid-range";
  return String(semver.satisfies(v, r, true));
});
const got = execFileSync(bin, ["__debug", "satisfies"], { input: cases.map((c) => c.join("\t")).join("\n") + "\n", encoding: "utf8", maxBuffer: 1 << 28 })
  .trim()
  .split("\n");
let failures = 0;
for (let i = 0; i < cases.length; i++) {
  if (got[i] !== expected[i]) {
    if (failures++ < 20) console.log(`semver mismatch: satisfies(${JSON.stringify(cases[i][1])}, ${JSON.stringify(cases[i][0])}) node=${expected[i]} lockstep=${got[i]}`);
  }
}
console.log(`semver: ${cases.length} cases, ${failures} mismatches`);

const alphabet = "abcAB019._-~$@/+!é";
const names = Array.from({ length: count / 4 }, () => Array.from({ length: 1 + rand(8) }, () => alphabet[rand(alphabet.length)]).join(""));
const sorted = [...names].sort((a, b) => a.localeCompare(b, "en"));
const mine = execFileSync(bin, ["__debug", "collate"], { input: names.join("\n") + "\n", encoding: "utf8" }).trim().split("\n");
let collateFailures = 0;
for (let i = 0; i < sorted.length; i++) {
  if (sorted[i] !== mine[i] && sorted[i].localeCompare(mine[i], "en") !== 0) {
    if (collateFailures++ < 10) console.log(`collation mismatch at ${i}: node=${JSON.stringify(sorted[i])} lockstep=${JSON.stringify(mine[i])}`);
  }
}
console.log(`collation: ${names.length} strings, ${collateFailures} mismatches`);
process.exit(failures || collateFailures ? 1 : 0);
