import { describe, expect, test } from "bun:test";
import { BASE_LIMIT, add, isPositive, sumTo } from "./calc.ts";
import { isEven } from "../app/(group)/route.ts";

describe("calc", () => {
  test("add", () => {
    expect(add(2, 3)).toBe(5);
  });
  test("base limit", () => {
    expect(BASE_LIMIT).toBe(100);
  });
  test("isPositive", () => {
    expect(isPositive(2)).toBe(true);
    expect(isPositive(-2)).toBe(false);
    expect(isPositive(0)).toBe(false);
  });
  test("sumTo", () => {
    expect(sumTo(4)).toBe(10);
    expect(sumTo(0)).toBe(0);
  });
});

test("isEven", () => {
  expect(isEven(4)).toBe(true);
  expect(isEven(3)).toBe(false);
});
