#!/usr/bin/env node
/**
 * A file-level comment that tsc keeps.
 */
import type { Shape } from "../types.js";

export interface Helper {
  (shape: Shape | (() => void)): void;
}
// A line comment before a statement.
export const helper: Helper = (shape) => {
  if (typeof shape === "function") shape();
  else if (shape.area() > 0) for (const _ of [1]) if (_) shape.area();
};
export {};
