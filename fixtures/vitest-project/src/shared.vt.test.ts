import { expect, test } from "vitest";
import { roundHalf } from "./shared.ts";

test("ceils to half steps", () => {
  expect(roundHalf(1.3, "ceil")).toBe(1.5);
  expect(roundHalf(1.6, "ceil")).toBe(2);
});
