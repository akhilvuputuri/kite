#!/usr/bin/env node
// Prints one line per filesystem entry under each given root:
//   <path>\t<type>\t<mode>\t<sha256 | link target>
// Paths are relative to the project directory and sorted bytewise, so two
// snapshots of the same tree are identical text. Usage:
//   snapshot.mjs <project-dir> [subdir ...]   (default subdir: node_modules)
import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, readlinkSync } from "node:fs";
import { join } from "node:path";

const [project, ...roots] = process.argv.slice(2);
if (!project) {
  console.error("usage: snapshot.mjs <project-dir> [subdir ...]");
  process.exit(2);
}

const lines = [];
function walk(rel) {
  const abs = join(project, rel);
  let st;
  try {
    st = lstatSync(abs);
  } catch (error) {
    if (error.code === "ENOENT") return;
    throw error;
  }
  const mode = (st.mode & 0o7777).toString(8).padStart(4, "0");
  if (st.isSymbolicLink()) {
    lines.push(`${rel}\tlink\t${mode}\t${readlinkSync(abs)}`);
  } else if (st.isDirectory()) {
    lines.push(`${rel}\tdir\t${mode}\t-`);
    for (const name of readdirSync(abs)) walk(join(rel, name));
  } else if (st.isFile()) {
    const hash = createHash("sha256").update(readFileSync(abs)).digest("hex");
    lines.push(`${rel}\tfile\t${mode}\t${hash}`);
  } else {
    lines.push(`${rel}\tother\t${mode}\t-`);
  }
}

for (const root of roots.length ? roots : ["node_modules"]) walk(root);
lines.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
process.stdout.write(lines.join("\n") + (lines.length ? "\n" : ""));
