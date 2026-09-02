/** Module-scope expression: its mutants are STATIC (hit at load time). */
export const BASE_LIMIT = 10 * 10;

export function add(a: number, b: number): number {
  return a + b;
}

export function isPositive(n: number): boolean {
  return n > 0;
}

/** Not covered by any test: its mutants must survive. */
export function untestedMax(a: number, b: number): number {
  return a > b ? a : b;
}

/** Covered; the `i--` → `i++` mutant loops forever → Timeout. */
export function sumTo(n: number): number {
  let total = 0;
  for (let i = n; i > 0; i--) {
    total += i;
  }
  return total;
}
