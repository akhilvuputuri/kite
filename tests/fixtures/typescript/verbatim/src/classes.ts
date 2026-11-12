import { type Shape, type Pair } from "./types.js";
import type { Mapped } from "./types.js";
import { helper, type Helper } from "./nested/helper.mjs";
import def, { type Tagged as T } from "./values.js";
import * as everything from "./values.js";

export abstract class Base implements Shape {
  static count = 0;
  static #registry = new Map<string, Base>();
  static {
    Base.count = Base.#registry.size;
  }
  declare readonly brand: "base";
  protected label!: string;
  optional?: number;
  untyped: unknown;
  #secret = 1;
  [key: string]: unknown;

  constructor(
    public readonly name: string,
    protected size = 1,
    private readonly tags: readonly string[] = [],
    plain?: number,
  ) {
    void plain;
  }

  abstract area(): number;
  describe(this: Base, prefix?: string): string;
  describe(prefix = ""): string {
    return `${prefix}${this.name}:${this.size}:${this.tags.length}:${this.#secret}`;
  }
  get secret(): number {
    return this.#secret;
  }
  set secret(value: number) {
    this.#secret = value;
  }
  protected optionalMethod?(): void;
}

export class Square extends Base {
  override label = "square";
  constructor(
    private side: number,
    public extra: Pair<number> = [side, side],
  ) {
    super("square", side);
    this.side = side * 1;
  }
  override area(): number {
    return this.side ** 2;
  }
  static of<const N extends number>(side: N): Square {
    return new Square(side);
  }
}

export const Anonymous = class<T> {
  constructor(readonly value: T) {}
};

export function use(): Mapped<Helper> | T | undefined {
  helper(def);
  return everything.flag ? undefined : {};
}
