type ImportOriginal = <T>() => T;
declare const importOriginal: ImportOriginal;

export const originalModule =
  importOriginal<typeof import("../ordinary/complete.js")>();
