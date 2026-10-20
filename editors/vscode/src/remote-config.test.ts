import { describe, expect, test } from "vitest";

import {
  isRemoteMode,
  parseRemoteEndpoint,
  remotePreviewHost,
  remotePreviewUrl,
} from "./remote-config.js";

describe("parseRemoteEndpoint", () => {
  test("parses websocket endpoints", () => {
    expect(parseRemoteEndpoint("ws://127.0.0.1:23626")).toMatchObject({
      kind: "ws",
      host: "127.0.0.1",
      port: 23626,
      display: "ws://127.0.0.1:23626",
    });
  });

  test("defaults the ports of websocket endpoints", () => {
    expect(parseRemoteEndpoint("ws://example.com").port).toBe(80);
    expect(parseRemoteEndpoint("wss://example.com").port).toBe(443);
  });

  test("parses tcp endpoints", () => {
    expect(parseRemoteEndpoint("tcp://example.com:23627")).toMatchObject({
      kind: "tcp",
      host: "example.com",
      port: 23627,
      display: "tcp://example.com:23627",
    });
  });

  test("rejects invalid endpoints", () => {
    expect(() => parseRemoteEndpoint("")).toThrow();
    expect(() => parseRemoteEndpoint("example.com:23626")).toThrow();
    expect(() => parseRemoteEndpoint("http://example.com:80")).toThrow();
    expect(() => parseRemoteEndpoint("tcp://example.com")).toThrow();
  });
});

describe("isRemoteMode", () => {
  test("detects a configured endpoint", () => {
    expect(isRemoteMode({ remoteEndpoint: "ws://example.com:23626" })).toBe(true);
  });

  test("ignores empty endpoints", () => {
    expect(isRemoteMode({})).toBe(false);
    expect(isRemoteMode({ remoteEndpoint: null })).toBe(false);
    expect(isRemoteMode({ remoteEndpoint: "  " })).toBe(false);
  });
});

describe("remotePreviewHost", () => {
  test("returns undefined in local mode", () => {
    expect(remotePreviewHost({})).toBeUndefined();
  });

  test("falls back to the endpoint host", () => {
    expect(remotePreviewHost({ remoteEndpoint: "ws://example.com:23626" })).toBe("example.com");
  });

  test("prefers the configured preview host", () => {
    expect(
      remotePreviewHost({
        remoteEndpoint: "ws://example.com:23626",
        remotePreviewHost: "preview.example.com",
      }),
    ).toBe("preview.example.com");
  });
});

test("builds preview urls", () => {
  expect(remotePreviewUrl("example.com", 23625, "ws")).toBe("ws://example.com:23625");
  expect(remotePreviewUrl("example.com", "23625", "http")).toBe("http://example.com:23625");
});
