import { Route } from "@solidjs/router";
import { screen, waitFor } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { closeSecretSheet, secretSheet } from "../state/secretSheet";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { SecretsView } from "./ShellSheets";

describe("/secrets", () => {
  it("opens the Add secret sheet and returns to the chat when it closes", async () => {
    stubFetch(() => ({ body: { items: [], nextBefore: null, cursor: null, hasMore: false } }));
    renderAt("/secrets", () => (
      <>
        <Route path="/" component={() => <p>Main chat</p>} />
        <Route path="/secrets" component={SecretsView} />
      </>
    ));
    await waitFor(() => expect(secretSheet()).not.toBeNull());
    closeSecretSheet();
    expect(await screen.findByText("Main chat")).toBeInTheDocument();
  });
});
