import { describe, expect, it } from "vitest";
import type { SecretProject } from "../api/secrets";
import {
  commandArgIndex,
  detectSecrets,
  isSecretCommand,
  maskSecrets,
  normalizeFolder,
  parseSecretCommand,
  resolveTarget,
  suggestName,
  targetCompletions,
} from "./secrets";

const PROJECTS: SecretProject[] = [
  { id: "p-poko", name: "Pokobot", slug: "pokobot", alias: "poko", environments: [{ slug: "dev", name: "Dev" }, { slug: "prod", name: "Prod" }] },
  { id: "p-orch", name: "Orchestra", slug: "orchestra-x1", alias: null, environments: [{ slug: "dev", name: "Dev" }] },
  { id: "p-luc", name: "Lucina", slug: "lucina", alias: "lucina", environments: [{ slug: "prod", name: "Prod" }] },
];

describe("/secret command", () => {
  it("is recognised only as a command", () => {
    for (const yes of ["/secret", "/secret A b", "  /SECRET A", "/Secret\tX"]) expect(isSecretCommand(yes), yes).toBe(true);
    for (const no of ["/secrets", "/secretary", "secret A b", "hi /secret", "/se", ""]) expect(isSecretCommand(no), no).toBe(false);
  });

  it("parses name, value and target and tracks the word being typed", () => {
    expect(parseSecretCommand("/secret POKO_KEY sk-abc poko/prod/Pokobot")).toEqual({
      name: "POKO_KEY",
      value: "sk-abc",
      target: "poko/prod/Pokobot",
      extra: false,
    });
    expect(parseSecretCommand("/secret K a b c d").extra).toBe(true);
    expect(commandArgIndex("/secret")).toBe(0);
    expect(commandArgIndex("/secret ")).toBe(1);
    expect(commandArgIndex("/secret K v")).toBe(2);
    expect(commandArgIndex("/secret K v po")).toBe(3);
    expect(commandArgIndex("hello")).toBe(0);
  });

  it("resolves aliases and paths", () => {
    expect(resolveTarget("poko", PROJECTS)?.project.id).toBe("p-poko");
    expect(resolveTarget("orchestra", PROJECTS)?.project.id).toBe("p-orch");
    const t = resolveTarget("poko/prod/Pokobot/sub/", PROJECTS)!;
    expect([t.project.id, t.environment, t.path]).toEqual(["p-poko", "prod", "/Pokobot/sub"]);
    expect(resolveTarget("poko/staging", PROJECTS)?.environment).toBeUndefined();
    expect(resolveTarget("luc", PROJECTS)?.project.id).toBe("p-luc");
    expect(resolveTarget("nope", PROJECTS)).toBeNull();
    expect(targetCompletions("p", PROJECTS)).toEqual(["poko"]);
    expect(targetCompletions("poko/", PROJECTS)).toEqual(["poko/dev", "poko/prod"]);
    expect(normalizeFolder("a/../b")).toBeNull();
    expect(normalizeFolder("")).toBe("/");
  });
});

describe("secret detector (client copy of Lucina's)", () => {
  const tokens = [
    "sk-proj-AbCdEfGhIjKlMnOpQrStUvWx0123456789",
    "ghp_ABCDEFGHIJKLMNOPQRSTuvwx1234",
    "xoxb-123456789012-abcdefghijkl",
    "AIzaSyA0123456789abcdefghijklmnopqrstuv",
    "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
    "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----",
    "hf_AbCdEfGhIjKlMnOpQrStUvWxYz0123456",
    "Q9zX4mL7pR2tV8wY3nB6cK1jH5gF0dS9aE7uI2oP",
  ];

  it("finds tokens and masks exactly them", () => {
    for (const tok of tokens) {
      const text = `ecco ${tok} grazie`;
      const spans = detectSecrets(text);
      expect(spans.length, tok).toBe(1);
      const masked = maskSecrets(text, spans);
      expect(masked).not.toContain(text.slice(spans[0]!.start, spans[0]!.end));
      expect(masked).toBe("ecco [secret:token] grazie");
    }
    const pair = "password=hunter2hunter2";
    expect(maskSecrets(pair)).toBe("password=[secret:token]");
  });

  it("leaves ordinary chat alone", () => {
    for (const text of [
      "Ricordami di comprare il latte domani alle 9",
      "550e8400-e29b-41d4-a716-446655440000",
      "01JABCDEFGHJKMNPQRSTVWXYZ0",
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
      "https://github.com/gvitolocs/SecretaryCode/pull/123/files",
      "secret://lucina/prod/lucina#OPEN_METEO_API_KEY",
      "[secret:token] is hidden",
      "La password di Gmail la trovi in Infisical",
      "ThisIsAnExtremelyLongCamelCaseIdentifierName",
      "gdrive:150qHWei8cXQbw1ssTSWFjEgC-XsmyKX3",
    ]) {
      expect(detectSecrets(text), text).toEqual([]);
    }
  });

  it("suggests names from the shape", () => {
    expect(suggestName("sk-ant-api03-x")).toBe("ANTHROPIC_API_KEY");
    expect(suggestName("ghp_x")).toBe("GITHUB_TOKEN");
    expect(suggestName("plain")).toBe("");
  });
});
