export type { Shape } from "./types.js";
export type * from "./types.js";
export * as types from "./types.js";
export { helper as renamed, type Helper } from "./nested/helper.mjs";
export type Tagged = `id-${number}`;

export const flag: boolean = true;
export const big = 1_000n;
export const separated = 1_000_000;
export const pattern = /a+b/giu;
export const tuple = [1, "two"] as const;
export const checked = { a: 1, b: "x" } satisfies Record<string, unknown>;
export let later!: string;
export const asserted = <unknown>flag as boolean;
export const nonNull = (globalThis as { maybe?: { deep?: string } }).maybe!.deep!;
export const chained = (globalThis as { maybe?: { deep?: () => string } }).maybe?.deep?.();

function generic<T>(value: T): T;
function generic<T>(value: T, other: T): T[];
function generic<T>(value: T, other?: T): T | T[] {
  return other === undefined ? value : [value, other];
}
export const instantiated = generic<string>;
export const called = generic<number>(1);
export const arrow = async <T,>(value: T): Promise<T> => value;
export function isString(value: unknown): value is string {
  return typeof value === "string";
}
export function assertString(value: unknown): asserts value is string {
  if (!isString(value)) throw new TypeError("not a string");
}
export function* numbers(limit: number): Generator<number, void, undefined> {
  for (let i: number = 0; i < limit; i++) yield i;
}
export async function* stream(): AsyncGenerator<string> {
  yield* ["a", "b"];
}
export function rest(first: number, ...others: number[]): number {
  let total: number = first;
  outer: for (const [index, value] of others.entries()) {
    if (index > 10) break outer;
    total += value;
  }
  try {
    JSON.parse("{");
  } catch (error: unknown) {
    total += error instanceof Error ? 1 : 0;
  } finally {
    total |= 0;
  }
  return total;
}
export const { a: renamedA, ...remaining }: { a: number; b: number } = { a: 1, b: 2 };
export const here = import.meta.url;
export const loaded = await Promise.resolve(separated);
export const lazy = () => import("./nested/helper.mjs");
export const text = `one ${separated} two ${`nested ${flag}`}`;
export const object = {
  method<T>(this: void, value: T): T {
    return value;
  },
  get computed(): number {
    return 1;
  },
  ["key" + 1]: 2 as number,
  flag,
};
export default function named(): void {}
