import {
  chmod,
  copyFile,
  cp,
  mkdir,
  readFile,
  writeFile,
} from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const npmRoot = fileURLToPath(new URL("..", import.meta.url));
const repositoryRoot = fileURLToPath(new URL("../..", import.meta.url));

type Options = Record<string, string>;
type PlatformOptions = {
  architecture: string;
  binary: string;
  operatingSystem: string;
  output: string;
  target: string;
  version: string;
};
type WrapperOptions = {
  output: string;
  version: string;
};
type WrapperPackageJson = {
  optionalDependencies: Record<string, string>;
  scripts?: Record<string, string>;
  version: string;
  [key: string]: unknown;
};

const platformPackageNames = [
  "@sacho/sacho-aarch64-apple-darwin",
  "@sacho/sacho-aarch64-pc-windows-msvc",
  "@sacho/sacho-aarch64-unknown-linux-musl",
  "@sacho/sacho-x86_64-apple-darwin",
  "@sacho/sacho-x86_64-pc-windows-msvc",
  "@sacho/sacho-x86_64-unknown-linux-musl",
];

function required(options: Options, name: string): string {
  const value = options[name];
  if (value === undefined || value === "") {
    throw new Error(`Missing --${name}.`);
  }
  return value;
}

function parseOptions(args: string[]): Options {
  const options: Options = {};
  for (let index = 0; index < args.length; index += 2) {
    const option = args[index];
    const value = args[index + 1];
    if (!option?.startsWith("--") || value === undefined) {
      throw new Error(`Invalid option list near ${option ?? "the end"}.`);
    }
    options[option.slice(2)] = value;
  }
  return options;
}

async function copyCommonFiles(output: string): Promise<void> {
  await copyFile(join(repositoryRoot, "README.md"), join(output, "README.md"));
  await copyFile(join(repositoryRoot, "LICENSE"), join(output, "LICENSE"));
}

async function writePackageJson(
  output: string,
  packageJson: Record<string, unknown>,
): Promise<void> {
  await writeFile(
    join(output, "package.json"),
    `${JSON.stringify(packageJson, null, 2)}\n`,
  );
}

export async function stageWrapper({
  output,
  version,
}: WrapperOptions): Promise<void> {
  await mkdir(output, { recursive: true });
  const packageJson = JSON.parse(
    await readFile(join(npmRoot, "package.json"), "utf8"),
  ) as WrapperPackageJson;
  packageJson.version = version;
  delete packageJson.scripts;
  for (const name of platformPackageNames) {
    packageJson.optionalDependencies[name] = version;
  }
  await writePackageJson(output, packageJson);
  await cp(join(npmRoot, "bin"), join(output, "bin"), { recursive: true });
  await cp(join(npmRoot, "lib"), join(output, "lib"), { recursive: true });
  await copyCommonFiles(output);
}

export async function stagePlatform({
  architecture,
  binary,
  operatingSystem,
  output,
  target,
  version,
}: PlatformOptions): Promise<void> {
  await mkdir(output, { recursive: true });
  const binaryName = basename(binary);
  const destination = join(output, binaryName);
  await copyFile(binary, destination);
  if (operatingSystem !== "win32") {
    await chmod(destination, 0o755);
  }
  await writePackageJson(output, {
    name: `@sacho/sacho-${target}`,
    version,
    description: `The Sacho binary for ${target}`,
    license: "GPL-3.0-only",
    author: "Hong Minhee <hong@minhee.org>",
    homepage: "https://github.com/dahlia/sacho",
    repository: {
      type: "git",
      url: "git+https://github.com/dahlia/sacho.git",
    },
    bugs: "https://github.com/dahlia/sacho/issues",
    os: [operatingSystem],
    cpu: [architecture],
    files: [binaryName],
    publishConfig: {
      access: "public",
      provenance: true,
    },
  });
  await copyCommonFiles(output);
}

async function main(): Promise<void> {
  const [kind, ...args] = process.argv.slice(2);
  const options = parseOptions(args);
  const output = resolve(required(options, "output"));
  const version = required(options, "version");
  if (kind === "wrapper") {
    await stageWrapper({ output, version });
  } else if (kind === "platform") {
    await stagePlatform({
      architecture: required(options, "cpu"),
      binary: resolve(required(options, "binary")),
      operatingSystem: required(options, "os"),
      output,
      target: required(options, "target"),
      version,
    });
  } else {
    throw new Error(`Unknown package kind: ${kind ?? "(missing)"}.`);
  }
}

if (process.argv[1] !== undefined && pathToFileURL(process.argv[1]).href === import.meta.url) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
