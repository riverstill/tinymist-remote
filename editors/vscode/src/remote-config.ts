/// Remote mode configuration shared by both the desktop and the web frontends.
///
/// This module must stay free of Node.js imports, because it is bundled into
/// the web extension as well.

import type { TinymistConfig } from "./config";

/**
 * A parsed remote endpoint of the tinymist server.
 */
export interface RemoteEndpoint {
  /**
   * The transport kind.
   */
  kind: "ws" | "tcp";
  /**
   * The normalized url of the endpoint.
   */
  url: string;
  /**
   * The host of the endpoint.
   */
  host: string;
  /**
   * The port of the endpoint.
   */
  port: number;
  /**
   * A human readable form of the endpoint, which never contains a token.
   */
  display: string;
}

/**
 * Whether the configuration points to a remote server.
 */
export function isRemoteMode(config: TinymistConfig): boolean {
  return typeof config.remoteEndpoint === "string" && !!config.remoteEndpoint.trim();
}

/**
 * Parses an endpoint such as `ws://127.0.0.1:23626` or
 * `tcp://127.0.0.1:23627`.
 */
export function parseRemoteEndpoint(endpoint: string): RemoteEndpoint {
  const raw = endpoint.trim();
  if (!raw) {
    throw new Error("the remote endpoint is empty");
  }

  let parsed: URL;
  try {
    parsed = new URL(raw);
  } catch {
    throw new Error(`invalid remote endpoint: ${raw}`);
  }

  const protocol = parsed.protocol.replace(/:$/, "");
  const host = parsed.hostname;
  if (!host) {
    throw new Error(`missing host in the remote endpoint: ${raw}`);
  }

  let kind: RemoteEndpoint["kind"];
  let defaultPort: number;
  switch (protocol) {
    case "ws":
      kind = "ws";
      defaultPort = 80;
      break;
    case "wss":
      kind = "ws";
      defaultPort = 443;
      break;
    case "tcp":
      kind = "tcp";
      defaultPort = 0;
      break;
    default:
      throw new Error(`unsupported protocol in the remote endpoint: ${raw}`);
  }

  const port = parsed.port ? Number(parsed.port) : defaultPort;
  if (!port) {
    throw new Error(`missing port in the remote endpoint: ${raw}`);
  }

  return {
    kind,
    url: parsed.toString(),
    host,
    port,
    display: `${protocol}://${host}:${port}`,
  };
}

/**
 * The host used to reach preview servers started by the remote tinymist
 * server, or `undefined` if the extension runs in local mode.
 */
export function remotePreviewHost(config: TinymistConfig): string | undefined {
  if (!isRemoteMode(config)) {
    return undefined;
  }

  const configured = config.remotePreviewHost?.trim();
  if (configured) {
    return configured;
  }

  try {
    return parseRemoteEndpoint(config.remoteEndpoint as string).host;
  } catch (e) {
    console.error("failed to parse the remote endpoint for the preview", e);
    return undefined;
  }
}

/**
 * Builds a URL pointing at a port of the remote host, such as
 * `ws://example.com:23625`.
 */
export function remotePreviewUrl(
  host: string,
  port: string | number,
  scheme: "ws" | "http",
): string {
  return `${scheme}://${host}:${port}`;
}

/**
 * Whether the next stop of a remote client is intentional, so that the
 * extension does not warn the user about a lost connection.
 */
let intentionalStop = false;

/**
 * Marks the remote client as intentionally stopped.
 */
export function markRemoteClientStopped(): void {
  intentionalStop = true;
}

/**
 * Resets the intentional stop marker for a new connection.
 */
export function resetRemoteClientStop(): void {
  intentionalStop = false;
}

/**
 * Whether the remote client is being stopped on purpose.
 */
export function isRemoteClientStopping(): boolean {
  return intentionalStop;
}
