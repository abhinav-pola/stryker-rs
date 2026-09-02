export function clamp(value: number, min: number, max: number): number {
  if (value < min) {
    return min;
  }
  if (value > max) {
    return max;
  }
  return value;
}

/** Untested: mutants here must be NoCoverage. */
export function untestedAbs(n: number): number {
  return n < 0 ? -n : n;
}
