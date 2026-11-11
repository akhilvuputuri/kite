export interface Thing {
  id: number;
}
export class Maker {
  make(): Thing {
    return { id: 1 };
  }
}
export const value = 1;
export const other = 2;
export default class Default {}
