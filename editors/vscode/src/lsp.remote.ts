/// This file provides the language client for a remotely deployed tinymist
/// server. It connects over WebSocket or raw TCP instead of spawning a local
/// process, so that the editor frontend and the language service can be
/// deployed separately.
///
/// Node.js-only: this module is imported by the desktop entry point only.

import * as net from "net";
import { Duplex } from "stream";

import * as vscode from "vscode";
import * as lc from "vscode-languageclient";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  StreamInfo,
} from "vscode-languageclient/node";
import type { BaseLanguageClient as CommonClient } from "vscode-languageclient";
import { WebSocket, type RawData } from "ws";

import type { TinymistConfig } from "./config";
import { l10nMsg } from "./l10n";
import {
  isRemoteClientStopping,
  parseRemoteEndpoint,
  resetRemoteClientStop,
  type RemoteEndpoint,
} from "./remote-config";

/**
 * A duplex stream bridging the `ws` package and the stream based language
 * client. Every websocket message carries a chunk of the LSP byte stream.
 */
class WebSocketDuplex extends Duplex {
  private readonly ws: WebSocket;

  constructor(ws: WebSocket) {
    super();
    this.ws = ws;
    ws.on("message", (data: RawData) => {
      const chunk = Array.isArray(data)
        ? Buffer.concat(data)
        : Buffer.isBuffer(data)
          ? data
          : Buffer.from(data);
      this.push(chunk);
    });
    ws.on("close", () => this.push(null));
    ws.on("error", (err) => this.destroy(err));
  }

  _read(): void {
    // The underlying websocket pushes data as it arrives.
  }

  _write(chunk: Buffer, _encoding: BufferEncoding, callback: (error?: Error | null) => void): void {
    this.ws.send(chunk, (err) => callback(err ?? null));
  }

  _final(callback: (error?: Error | null) => void): void {
    this.ws.close();
    callback();
  }

  _destroy(error: Error | null, callback: (error?: Error | null) => void): void {
    try {
      this.ws.terminate();
    } catch {
      // Ignore errors while tearing down a broken connection.
    }
    callback(error);
  }
}

function connectTcp(endpoint: RemoteEndpoint, token?: string): Promise<StreamInfo> {
  return new Promise<StreamInfo>((resolve, reject) => {
    const socket = net.connect({ host: endpoint.host, port: endpoint.port });
    socket.setNoDelay(true);

    const onError = (err: Error) => {
      socket.destroy();
      reject(err);
    };
    socket.once("error", onError);
    socket.once("connect", () => {
      socket.off("error", onError);
      if (token) {
        // The server consumes the authentication preamble before the LSP
        // stream starts.
        socket.write(`TINYMIST-AUTH ${token}\r\n`);
      }
      resolve({ reader: socket, writer: socket });
    });
  });
}

function connectWebSocket(endpoint: RemoteEndpoint, token?: string): Promise<StreamInfo> {
  return new Promise<StreamInfo>((resolve, reject) => {
    const headers: Record<string, string> = {};
    if (token) {
      headers["Authorization"] = `Bearer ${token}`;
    }

    const ws = new WebSocket(endpoint.url, { headers, handshakeTimeout: 10_000 });
    const onError = (err: Error) => {
      ws.terminate();
      reject(err);
    };
    ws.once("error", onError);
    ws.once("open", () => {
      ws.off("error", onError);
      const stream = new WebSocketDuplex(ws);
      resolve({ reader: stream, writer: stream });
    });
  });
}

function connect(endpoint: RemoteEndpoint, token?: string): Promise<StreamInfo> {
  return endpoint.kind === "tcp" ? connectTcp(endpoint, token) : connectWebSocket(endpoint, token);
}

/**
 * Creates a language client connected to a remotely deployed tinymist server.
 */
export async function createRemoteLanguageClient(
  _context: vscode.ExtensionContext,
  config: TinymistConfig,
  clientOptions: LanguageClientOptions,
): Promise<CommonClient> {
  const endpoint = parseRemoteEndpoint(config.remoteEndpoint ?? "");
  const token = config.remoteToken?.trim() || undefined;
  resetRemoteClientStop();

  const serverOptions: ServerOptions = () => connect(endpoint, token);
  const client = new LanguageClient(
    "tinymist",
    "Tinymist Typst Language Server",
    serverOptions,
    clientOptions,
  );

  let hasConnected = false;
  client.onDidChangeState((event) => {
    if (event.newState === lc.State.Running) {
      hasConnected = true;
      return;
    }
    if (event.newState !== lc.State.Stopped || !hasConnected || isRemoteClientStopping()) {
      return;
    }

    hasConnected = false;
    void vscode.window
      .showWarningMessage(
        l10nMsg("Lost the connection to the tinymist server at {0}.", {
          0: endpoint.display,
        }),
        l10nMsg("Restart Server"),
        l10nMsg("Show Log"),
      )
      .then((choice) => {
        if (choice === l10nMsg("Restart Server")) {
          void vscode.commands.executeCommand("tinymist.restartServer");
        } else if (choice === l10nMsg("Show Log")) {
          void vscode.commands.executeCommand("tinymist.showLog");
        }
      });
  });

  return client;
}
