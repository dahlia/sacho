import assert from "node:assert/strict";
import { access, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { stagePlatform, stageWrapper } from "../scripts/stage.mts";

test("stages the wrapper at the requested version", async (context) => {
  const temporary = await mkdtemp(join(tmpdir(), "sacho-npm-wrapper-"));
  context.after(() => rm(temporary, { force: true, recursive: true }));
  const output = join(temporary, "package");
  await stageWrapper({ output, version: "1.2.3" });

  const packageJson = JSON.parse(
    await readFile(join(output, "package.json"), "utf8"),
  );
  assert.equal(packageJson.name, "@sacho/sacho");
  assert.equal(packageJson.version, "1.2.3");
  for (const version of Object.values(packageJson.optionalDependencies)) {
    assert.equal(version, "1.2.3");
  }
  await access(join(output, "bin/sacho.js"));
  await access(join(output, "lib/platform.js"));
  await access(join(output, "README.md"));
  await access(join(output, "LICENSE"));
});

test("stages a platform-specific binary package", async (context) => {
  const temporary = await mkdtemp(join(tmpdir(), "sacho-npm-platform-"));
  context.after(() => rm(temporary, { force: true, recursive: true }));
  const binary = join(temporary, "sacho");
  const output = join(temporary, "package");
  await writeFile(binary, "test binary");
  await stagePlatform({
    architecture: "x64",
    binary,
    operatingSystem: "linux",
    output,
    target: "x86_64-unknown-linux-musl",
    version: "1.2.3",
  });

  const packageJson = JSON.parse(
    await readFile(join(output, "package.json"), "utf8"),
  );
  assert.equal(packageJson.name, "@sacho/sacho-x86_64-unknown-linux-musl");
  assert.equal(packageJson.version, "1.2.3");
  assert.deepEqual(packageJson.os, ["linux"]);
  assert.deepEqual(packageJson.cpu, ["x64"]);
  assert.deepEqual(packageJson.files, ["sacho"]);
  assert.equal(await readFile(join(output, "sacho"), "utf8"), "test binary");
});
