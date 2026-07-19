"use strict";

const assert = require("node:assert/strict");
const { chmod, mkdir, mkdtemp, rm, writeFile } = require("node:fs/promises");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");
const { binaryFor, packageFor, targetFor } = require("../lib/platform.js");

const cases = [
  ["darwin", "arm64", "aarch64-apple-darwin"],
  ["darwin", "x64", "x86_64-apple-darwin"],
  ["linux", "arm64", "aarch64-unknown-linux-musl"],
  ["linux", "x64", "x86_64-unknown-linux-musl"],
  ["win32", "arm64", "aarch64-pc-windows-msvc"],
  ["win32", "x64", "x86_64-pc-windows-msvc"],
];

test("maps npm platforms to Rust targets", () => {
  for (const [platform, architecture, target] of cases) {
    assert.equal(targetFor(platform, architecture), target);
    assert.equal(packageFor(platform, architecture), `@sacho/sacho-${target}`);
  }
});

test("rejects unsupported platforms", () => {
  assert.throws(
    () => targetFor("freebsd", "x64"),
    /Sacho does not provide a binary for freebsd x64\./,
  );
});

test(
  "the launcher forwards arguments and exit status",
  { skip: process.platform === "win32" },
  async (context) => {
    const temporary = await mkdtemp(join(tmpdir(), "sacho-npm-launcher-"));
    context.after(() => rm(temporary, { force: true, recursive: true }));
    const nodeModules = join(temporary, "node_modules");
    const packageDirectory = join(nodeModules, ...packageFor().split("/"));
    await mkdir(packageDirectory, { recursive: true });
    const binary = join(packageDirectory, binaryFor());
    await writeFile(binary, '#!/bin/sh\nprintf "%s\\n" "$@"\nexit 23\n');
    await chmod(binary, 0o755);

    const result = spawnSync(
      process.execPath,
      [join(__dirname, "../bin/sacho.js"), "alpha", "two words"],
      {
        encoding: "utf8",
        env: { ...process.env, NODE_PATH: nodeModules },
      },
    );
    assert.equal(result.status, 23);
    assert.equal(result.stdout, "alpha\ntwo words\n");
    assert.equal(result.stderr, "");
  },
);
