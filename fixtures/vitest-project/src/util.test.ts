import { describe, expect, test } from "vitest";
import { clamp } from "./util.ts";

describe("clamp", () => {
  test("clamps low", () => {
    expect(clamp(-5, 0, 10)).toBe(0);
  });
  test("clamps high", () => {
    expect(clamp(15, 0, 10)).toBe(10);
  });
  test("passes through", () => {
    expect(clamp(5, 0, 10)).toBe(5);
  });
});
