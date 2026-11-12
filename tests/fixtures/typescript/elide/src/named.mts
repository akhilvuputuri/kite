// No imports or exports, but an .mts file is a module by its name: tsc ends it with
// `export {}` and adds no "use strict".
const named: number = 1;
(globalThis as { kiteNamed?: number }).kiteNamed = named;
