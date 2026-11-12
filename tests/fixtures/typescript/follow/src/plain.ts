// Forced to be a module: tsc ends it with `export {}` and adds no "use strict".
const plain: number = 1;
(globalThis as { kitePlain?: number }).kitePlain = plain;
