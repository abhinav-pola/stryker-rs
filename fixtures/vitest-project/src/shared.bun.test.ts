import { expect, test } from "bun:test";
import { roundHalf } from "./shared.ts";

test("floors to half steps", () => {
  expect(roundHalf(1.3, "floor")).toBe(1);
  expect(roundHalf(1.6, "floor")).toBe(1.5);
});
