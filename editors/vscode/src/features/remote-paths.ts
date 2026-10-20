/// This file provides path mapping between the local workspace and a remote
/// tinymist server. It is a no-op unless `tinymist.remote.pathMap` is
/// configured.

import * as vscode from "vscode";

import type { RemotePathMapping } from "../config";

const LOCAL = "local";
const REMOTE = "remote";

let mappings: RemotePathMapping[] = [];

/**
 * Sets the path mappings used by the remote mode.
 */
export function setRemotePathMappings(next?: RemotePathMapping[] | null): void {
  mappings = (next ?? []).filter(
    (mapping) =>
      mapping &&
      typeof mapping.local === "string" &&
      typeof mapping.remote === "string" &&
      !!mapping.local.trim() &&
      !!mapping.remote.trim(),
  );
}

/**
 * Whether any path mapping is configured.
 */
export function hasRemotePathMappings(): boolean {
  return mappings.length > 0;
}

function normalize(path: string): string {
  return path.replace(/\\/g, "/");
}

function trimTrailingSlash(path: string): string {
  return path.replace(/\/+$/, "");
}

function mapPath(
  path: string,
  from: typeof LOCAL | typeof REMOTE,
  to: typeof LOCAL | typeof REMOTE,
): string {
  if (!hasRemotePathMappings()) {
    return path;
  }

  const normalized = normalize(path);
  const lower = normalized.toLowerCase();

  for (const mapping of mappings) {
    const source = trimTrailingSlash(normalize(mapping[from]));
    const target = trimTrailingSlash(normalize(mapping[to]));
    if (!source) {
      continue;
    }

    const sourceLower = source.toLowerCase();
    if (lower === sourceLower) {
      return target;
    }
    if (lower.startsWith(sourceLower + "/")) {
      return target + normalized.slice(source.length);
    }
  }

  return path;
}

/**
 * Maps a local path to the path seen by the remote server.
 */
export function toRemotePath(path: string): string {
  return mapPath(path, LOCAL, REMOTE);
}

/**
 * Maps a path seen by the remote server to a local path.
 */
export function toLocalPath(path: string): string {
  return mapPath(path, REMOTE, LOCAL);
}

/**
 * Maps a URI received from the remote server to a local URI.
 */
export function remoteUriToLocalUri(uri: string | vscode.Uri): vscode.Uri {
  const parsed = typeof uri === "string" ? vscode.Uri.parse(uri) : uri;
  if (!hasRemotePathMappings() || parsed.scheme !== "file") {
    return parsed;
  }

  const mapped = toLocalPath(parsed.path);
  if (mapped === parsed.path) {
    return parsed;
  }
  return vscode.Uri.file(mapped);
}

/**
 * Maps a local URI to the URI form the remote server expects.
 */
export function localUriToRemoteUri(uri: string | vscode.Uri): string {
  const parsed = typeof uri === "string" ? vscode.Uri.parse(uri) : uri;
  if (!hasRemotePathMappings() || parsed.scheme !== "file") {
    return parsed.toString();
  }

  // A Windows file URI has a leading slash before the drive letter, such as
  // `/c:/notes`, while the mapping is usually written as `C:\notes`.
  const candidates = [parsed.path, parsed.path.replace(/^\/([a-zA-Z]:)/, "$1"), parsed.fsPath];

  for (const candidate of candidates) {
    const mapped = toRemotePath(candidate);
    if (mapped !== candidate) {
      const path = mapped.startsWith("/") ? mapped : "/" + mapped;
      return parsed.with({ path }).toString();
    }
  }

  return parsed.toString();
}
