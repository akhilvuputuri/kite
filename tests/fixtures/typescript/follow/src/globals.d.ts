// A declaration file emits nothing, but the source file it imports is compiled.
import type { FromDts } from "./from-dts.js";
declare global {
  var kiteGlobal: FromDts | undefined;
}
