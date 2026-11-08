// The buttons of a card the server closed: off on every client, whoever closed it.
import { describe, expect, it } from "vitest";
import { parseCards } from "../api/cards";

describe("parseCards", () => {
  it("keeps closed cards, with or without a button", () => {
    expect(
      parseCards({
        cards: {
          a: { closed: true, status: "decided", action: "dc-1-2" },
          b: { closed: true, status: "withdrawn" },
          c: { closed: false },
          d: "junk",
        },
      }),
    ).toEqual({ a: { closed: true, status: "decided", action: "dc-1-2" }, b: { closed: true, status: "withdrawn" } });
  });

  it("answers nothing for an older server", () => {
    expect(parseCards(null)).toEqual({});
    expect(parseCards({})).toEqual({});
  });
});
