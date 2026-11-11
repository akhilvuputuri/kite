// A script, not a module: nothing is imported or exported. `strict` makes tsc open it
// with "use strict".
const scriptValue: number = 0x10;
(globalThis as { kiteScript?: number }).kiteScript = scriptValue;
