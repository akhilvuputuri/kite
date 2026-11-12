// With verbatimModuleSyntax each of these still loads its module.
import {} from "./values.js";
export {} from "./side.js";
export { type Shape, type Pair as Couple } from "./types.js";
declare global {
  namespace KiteAmbient {
    interface Settings {
      mode: string;
    }
    enum Level {
      Low,
    }
  }
}
export const mode: KiteAmbient.Settings["mode"] = "x";
