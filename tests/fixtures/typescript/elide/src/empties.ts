// Without verbatimModuleSyntax none of these loads its module.
import {} from "./side-effect.js";
export {} from "./side-effect.js";
export { type Thing } from "./lib.js";
import { Maker } from "./lib.js";
export const huge = 0x10n;
export function describe(maker: Maker): string {
  return typeof maker;
}
