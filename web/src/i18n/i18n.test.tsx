import { render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { dictionaries } from "./dictionaries";
import {
  detectLocale,
  detectedLocale,
  LOCALE_KEY,
  locale,
  localeOverride,
  matchLocale,
  reloadLocale,
  setLocale,
  speechLang,
  t,
} from "./index";

describe("locale detection", () => {
  it("takes the first supported language from navigator.languages", () => {
    expect(detectLocale(["it-IT", "en-US"])).toBe("it");
    expect(detectLocale(["fr-FR", "da-DK", "en"])).toBe("da");
    expect(detectLocale(["EN_gb"])).toBe("en");
  });

  it("falls back to English for unsupported or missing languages", () => {
    expect(detectLocale(["fr-FR", "de"])).toBe("en");
    expect(detectLocale(["nb-NO", "sv"])).toBe("en"); // Norwegian and Swedish are not Danish.
    expect(detectLocale([])).toBe("en");
    expect(matchLocale("")).toBeNull();
  });

  it("reads the browser's languages", () => {
    vi.stubGlobal("navigator", { languages: ["da-DK", "en"], language: "da-DK" });
    reloadLocale();
    expect(detectedLocale()).toBe("da");
    expect(locale()).toBe("da");
    expect(document.documentElement.lang).toBe("da");
    vi.stubGlobal("navigator", { languages: [], language: "it-IT" });
    reloadLocale();
    expect(locale()).toBe("it");
  });
});

describe("language override", () => {
  afterEach(() => setLocale(null));

  it("wins over the device language, is stored, and syncs <html lang>", () => {
    reloadLocale(["it-IT"]);
    expect(locale()).toBe("it");
    setLocale("da");
    expect(locale()).toBe("da");
    expect(localeOverride()).toBe("da");
    expect(localStorage.getItem(LOCALE_KEY)).toBe("da");
    expect(document.documentElement.lang).toBe("da");
    expect(speechLang()).toBe("da-DK");

    // Survives a reload of the app.
    reloadLocale(["it-IT"]);
    expect(locale()).toBe("da");

    setLocale(null);
    expect(localStorage.getItem(LOCALE_KEY)).toBeNull();
    expect(locale()).toBe("it");
    expect(speechLang()).toBe("it-IT");
  });

  it("ignores a stored value it does not know", () => {
    localStorage.setItem(LOCALE_KEY, "klingon");
    reloadLocale(["en-US"]);
    expect(localeOverride()).toBeNull();
    expect(locale()).toBe("en");
  });

  it("works when storage throws", () => {
    const get = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    const set = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    reloadLocale(["en-US"]);
    setLocale("it");
    expect(locale()).toBe("it");
    get.mockRestore();
    set.mockRestore();
  });
});

describe("t()", () => {
  afterEach(() => setLocale(null));

  it("translates, interpolates and formats numbers per language", () => {
    expect(t("chat.send")).toBe("Send");
    setLocale("it");
    expect(t("chat.send")).toBe("Invia");
    expect(t("login.tryAgainIn", { time: "0:03" })).toBe("Riprova tra 0:03.");
    setLocale("da");
    expect(t("shell.healthSlow", { ms: 4200 })).toBe("Lucina er langsom (4.200 ms)");
  });

  it("picks plural forms", () => {
    expect(t("shell.passwordHint", { count: 1 })).toBe("At least 1 character.");
    expect(t("shell.passwordHint", { count: 8 })).toBe("At least 8 characters.");
    setLocale("it");
    expect(t("shell.passwordHint", { count: 1 })).toBe("Almeno 1 carattere.");
    expect(t("shell.passwordHint", { count: 8 })).toBe("Almeno 8 caratteri.");
  });

  it("falls back to English when a translation is missing", () => {
    const it_ = dictionaries.chat.it as Record<string, unknown>;
    const saved = it_.send;
    delete it_.send;
    try {
      setLocale("it");
      expect(t("chat.send")).toBe("Send");
    } finally {
      it_.send = saved;
    }
  });

  it("only accepts known keys", () => {
    // @ts-expect-error: not a key
    expect(t("chat.nope")).toBe("chat.nope");
  });

  it("re-renders when the language changes", () => {
    render(() => <p>{t("chat.emptyTitle")}</p>);
    expect(screen.getByText("Hello. Lucina is listening.")).toBeInTheDocument();
    setLocale("it");
    expect(screen.getByText("Ciao. Lucina ti ascolta.")).toBeInTheDocument();
  });

  it("has the same keys in every language", () => {
    for (const [feature, dict] of Object.entries(dictionaries)) {
      const keys = Object.keys(dict.en).sort();
      expect(Object.keys(dict.it).sort(), `${feature}.it`).toEqual(keys);
      expect(Object.keys(dict.da).sort(), `${feature}.da`).toEqual(keys);
    }
  });
});

describe("language picker", () => {
  afterEach(() => setLocale(null));

  it("is in the account panel and switches the whole UI", async () => {
    const { AccountDialog } = await import("../components/AccountDialog");
    const { fireEvent } = await import("@solidjs/testing-library");
    render(() => <AccountDialog open={true} onClose={() => {}} />);
    const select = screen.getByLabelText("Language") as HTMLSelectElement;
    expect(select.value).toBe("auto");
    expect(screen.getByRole("option", { name: "Automatic (English)" })).toBeInTheDocument();

    fireEvent.change(select, { target: { value: "da" } });
    expect(locale()).toBe("da");
    expect(screen.getByRole("heading", { name: "Konto" })).toBeInTheDocument();
    expect(document.documentElement.lang).toBe("da");

    fireEvent.change(screen.getByLabelText("Sprog"), { target: { value: "auto" } });
    expect(localeOverride()).toBeNull();
    expect(screen.getByRole("heading", { name: "Account" })).toBeInTheDocument();
  });
});
