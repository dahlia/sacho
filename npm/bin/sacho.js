#!/usr/bin/env node
"use strict";

const { spawnSync } = require("node:child_process");
const { binaryFor, packageFor } = require("../lib/platform.js");

function fail(message) {
  console.error(`sacho: ${message}`);
  process.exit(1);
}

let binaryName;
let packageName;
try {
  binaryName = binaryFor();
  packageName = packageFor();
} catch (error) {
  fail(error.message);
}

let binaryPath;
try {
  binaryPath = require.resolve(`${packageName}/${binaryName}`);
} catch {
  fail(
    `The ${packageName} optional package is missing. ` +
      "Reinstall @sacho/sacho without omitting optional dependencies.",
  );
}

const result = spawnSync(binaryPath, process.argv.slice(2), { stdio: "inherit" });
if (result.error !== undefined) {
  fail(`Unable to run ${binaryPath}: ${result.error.message}`);
}
if (result.signal !== null) {
  process.kill(process.pid, result.signal);
  process.exit(1);
}
process.exit(result.status ?? 1);
