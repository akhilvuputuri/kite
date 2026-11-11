// No imports and no exports. Under module NodeNext in a "type": "module" package this is
// still a module, so tsc ends it with `export {}`.
const greeting: string = "side";
(globalThis as { kiteSide?: string }).kiteSide = greeting;
