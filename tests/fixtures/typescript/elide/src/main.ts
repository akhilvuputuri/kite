// Without verbatimModuleSyntax, tsc drops every import that is never used as a value.
import { Thing, Maker, value, other } from "./lib.js";
import Default from "./lib.js";
import * as asTypes from "./lib.js";
import * as asValues from "./lib.js";
import TypeOnlyDefault, { Maker as TypeOnlyMaker } from "./lib.js";
import "./side-effect.js";

/** Removed by removeComments. */
export function build(maker: Maker, shape: asTypes.Thing): Thing {
  const made: TypeOnlyMaker | TypeOnlyDefault | undefined = undefined;
  void made;
  void shape;
  return maker.make();
}
export const kept = [value, new Default(), asValues.other];
export type Unused = typeof other;

export class Resource {
  accessor count = 0;
  [Symbol.dispose](): void {}
}
export function scoped(): number {
  using resource = new Resource();
  return resource.count;
}
