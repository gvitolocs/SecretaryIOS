import { describe, expect, it } from "vitest";
import { parseItem, type ItemMeta, type TimelineItem } from "../api/timeline";
import { matchesFilter, mcpSource, SOURCE_FILTERS, sourceLine } from "./timeline";

describe("mcpSource", () => {
  it.each([
    ["claude-code", "Claude Code"],
    [" Claude-Code-terminal ", "Claude Code"],
    ["claude code", "Claude Code"],
    ["claude-desktop", "Claude Desktop"],
    ["claude-desktop-other", "Claude"],
    ["claude", "Claude"],
    ["chatgpt-browser", "ChatGPT"],
    ["openai-api", "ChatGPT"],
    ["grok", "Grok"],
    ["xai-client", "Grok"],
    ["codex-cli", "Codex"],
    ["cursor-editor", "Cursor"],
    [" WWW ", "Cursor"],
    ["gemini-cli", "Gemini"],
    ["test", "MCP · test"],
    [" Custom Client ", "MCP ·  Custom Client "],
    ["", "MCP"],
    ["  ", "MCP"],
    [undefined, "MCP"],
  ])("labels caller %s as %s for both roles", (caller, label) => {
    for (const role of ["user", "lucina"]) {
      expect(mcpSource(role, "Hello", { via: "mcp", caller })).toEqual({ caller: caller ?? null, label, text: "Hello" });
    }
  });

  it("strips only a legacy user prefix with a closing bracket and space", () => {
    expect(mcpSource("user", "[via Lucina MCP, from  claude-code ] Ciao Lucina", undefined)).toEqual({
      caller: "claude-code", label: "Claude Code", text: "Ciao Lucina",
    });
    expect(mcpSource("user", "[via Lucina MCP, from ] Hi", {})).toEqual({ caller: "", label: "MCP", text: "Hi" });
    for (const text of ["[via Lucina MCP, from claude-code]Hi", "[via Lucina MCP, from claude-code", " [via Lucina MCP, from claude-code] Hi"]) {
      expect(mcpSource("user", text)).toBeNull();
    }
  });

  it("uses explicit via metadata before the legacy fallback", () => {
    const text = "[via Lucina MCP, from claude-code] Hi";
    expect(mcpSource("lucina", text)).toBeNull();
    expect(mcpSource("user", text, { via: "web" })).toBeNull();
    expect(mcpSource("user", text, { via: "" })).toBeNull();
    expect(mcpSource("user", text, { via: "mcp", caller: "www" })).toEqual({ caller: "www", label: "Cursor", text });
  });

  it("leaves real app messages alone even with caller metadata", () => {
    expect(mcpSource("user", "Hello")).toBeNull();
    expect(mcpSource("lucina", "Hello", { caller: "chatgpt" })).toBeNull();
    expect(sourceLine("web", undefined)).toBe("Web");
    expect(sourceLine("web", { via: "mcp", caller: "www" })).toBe("Cursor");
    expect(sourceLine("web", undefined, "user", "[via Lucina MCP, from claude-code] Hi")).toBe("Claude Code");
  });

  it("preserves caller identity through network parsing", () => {
    const item = parseItem({ id: "1", ts: 1, channel: "web", role: "lucina", text: "Hi", meta: { via: "mcp", caller: " Custom Client " } });
    expect(item?.meta).toEqual({ via: "mcp", caller: " Custom Client " });
    expect(mcpSource(item!.role, item!.text, item!.meta)?.label).toBe("MCP ·  Custom Client ");
    expect(parseItem({ id: "2", ts: 1, channel: "web", role: "user", meta: { via: 1, caller: {} } })?.meta).toBeUndefined();
  });
});

describe("source filters", () => {
  const item = (channel: TimelineItem["channel"], meta?: ItemMeta, text = "Hello", role: TimelineItem["role"] = "user") => ({ channel, meta, text, role });

  it("includes the Agents filter", () => {
    expect(SOURCE_FILTERS).toContainEqual({ id: "mcp", label: "chat.filterMcp" });
  });

  it("excludes both MCP roles and legacy turns from App and includes them in Agents", () => {
    for (const message of [item("web", { via: "mcp", caller: "claude-code" }), item("web", { via: "mcp" }, "Hi", "lucina"), item("web", undefined, "[via Lucina MCP, from www] Hi"), item("web", { via: "mcp", kind: "proactive" })]) {
      expect(matchesFilter(message, "all")).toBe(true);
      expect(matchesFilter(message, "app")).toBe(false);
      expect(matchesFilter(message, "mcp")).toBe(true);
    }
  });

  it("preserves app, proactive and other channel filters for non-MCP items", () => {
    for (const message of [item("web"), item("ios"), item("other", { kind: "proactive" })]) {
      expect(matchesFilter(message, "app")).toBe(true);
      expect(matchesFilter(message, "mcp")).toBe(false);
    }
    for (const channel of ["telegram", "discord", "alexa"] as const) {
      expect(matchesFilter(item(channel), channel)).toBe(true);
      expect(matchesFilter(item(channel), "app")).toBe(false);
    }
  });
});
