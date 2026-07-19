import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { access, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { promisify } from "node:util";
import { stagePlatform, stageWrapper } from "../scripts/stage.mts";

const execFileAsync = promisify(execFile);

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
  assert.equal(packageJson.homepage, "https://sacho.dev/");
  for (const version of Object.values(packageJson.optionalDependencies)) {
    assert.equal(version, "1.2.3");
  }
  await access(join(output, "bin/sacho.js"));
  await access(join(output, "lib/platform.js"));
  await access(join(output, "skills/sacho/SKILL.md"));
  await access(join(output, "README.md"));
  await access(join(output, "LICENSE"));
});

test("publishes the bundled skill in the wrapper tarball", async (context) => {
  const temporary = await mkdtemp(join(tmpdir(), "sacho-npm-tarball-"));
  context.after(() => rm(temporary, { force: true, recursive: true }));
  const output = join(temporary, "package");
  await stageWrapper({ output, version: "1.2.3" });

  // Pack the staged wrapper exactly as `npm publish` would, so this guards the
  // whole chain: a skill copied into the stage but dropped from the `files`
  // allowlist would be silently omitted from the published package.
  //
  // Run npm through Node rather than spawning `npm` directly: on Windows npm is
  // exposed as `npm.cmd`, which execFile cannot launch. Under `npm test`,
  // npm_execpath points at npm's JavaScript CLI, so `node <cli>` is portable on
  // every platform. Require it instead of guessing a launcher; this suite runs
  // via `npm test`, and a clear message beats a fallback that breaks on Windows.
  const npmExecpath = process.env.npm_execpath;
  assert.match(
    npmExecpath ?? "",
    /\.[cm]?js$/,
    "run this test via `npm test` so npm_execpath points at npm's JavaScript CLI",
  );
  const { stdout } = await execFileAsync(
    process.execPath,
    [npmExecpath, "pack", "--dry-run", "--json", output],
    { cwd: temporary },
  );
  const [report] = JSON.parse(stdout);
  const files = report.files.map((entry) => entry.path);
  assert.ok(
    files.includes("skills/sacho/SKILL.md"),
    `published tarball is missing the skill; packed: ${files.join(", ")}`,
  );
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
  assert.equal(packageJson.homepage, "https://sacho.dev/");
  assert.deepEqual(packageJson.os, ["linux"]);
  assert.deepEqual(packageJson.cpu, ["x64"]);
  assert.deepEqual(packageJson.files, ["sacho"]);
  assert.equal(await readFile(join(output, "sacho"), "utf8"), "test binary");
});
