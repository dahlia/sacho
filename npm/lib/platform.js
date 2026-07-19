"use strict";

const targets = Object.freeze({
  "darwin:arm64": "aarch64-apple-darwin",
  "darwin:x64": "x86_64-apple-darwin",
  "linux:arm64": "aarch64-unknown-linux-musl",
  "linux:x64": "x86_64-unknown-linux-musl",
  "win32:arm64": "aarch64-pc-windows-msvc",
  "win32:x64": "x86_64-pc-windows-msvc",
});

function targetFor(platform = process.platform, architecture = process.arch) {
  const target = targets[`${platform}:${architecture}`];
  if (target === undefined) {
    throw new Error(
      `Sacho does not provide a binary for ${platform} ${architecture}.`,
    );
  }
  return target;
}

function packageFor(platform = process.platform, architecture = process.arch) {
  return `@sacho/sacho-${targetFor(platform, architecture)}`;
}

function binaryFor(platform = process.platform) {
  return platform === "win32" ? "sacho.exe" : "sacho";
}

module.exports = { binaryFor, packageFor, targetFor };
