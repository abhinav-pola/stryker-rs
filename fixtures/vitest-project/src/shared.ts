/** Covered by BOTH runtimes: bun tests the floor, vitest tests the ceil. */
export function roundHalf(n: number, mode: "floor" | "ceil"): number {
  if (mode === "floor") {
    return Math.floor(n * 2) / 2;
  }
  return Math.ceil(n * 2) / 2;
}
