export interface Shape {
  readonly name: string;
  area(): number;
}
export type Pair<A, B = A> = readonly [A, B];
export type Mapped<T> = { [K in keyof T]?: T[K] extends Function ? never : T[K] };
export type Tagged = `id-${number}`;
export declare const ambient: number;
export declare function ambientCall(x: number): void;
declare global {
  interface Array<T> {
    last?: T;
  }
}
export declare enum Ambient {
  A,
}
export declare namespace Declared {
  const inside: number;
}
