type ImportOriginal = <T>() => T;
declare const importOriginal: ImportOriginal;

const originalModule =
  importOriginal<typeof import("../ordinary/complete.js")>();

export function readsOriginalModule(): unknown {
  return originalModule;
}
