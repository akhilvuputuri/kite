import { fromUtil } from "./util.js";
import type { OnlyType } from "./only-type.js";
import { mapped } from "@lib/mapped";
import { excluded } from "./excluded";
import "./plain.js";

export const all: OnlyType = [fromUtil, mapped, excluded];
export type Lazy = typeof import("./lazy-type.js");
