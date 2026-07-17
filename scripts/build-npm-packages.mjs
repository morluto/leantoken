#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPOSITORY = "https://github.com/morluto/leantoken";
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export const PLATFORMS = JSON.parse(
  await readFile(join(ROOT, "npm", "platforms.json"), "utf8"),
);

function run(program, args, options = {}) {
  const result = spawnSync(program, args, {
    encoding: "utf8",
    stdio: options.capture ? "pipe" : "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = options.capture ? `: ${(result.stderr || result.stdout).trim()}` : "";
    throw new Error(`${program} exited with status ${result.status}${detail}`);
  }
  return result.stdout?.trim() ?? "";
}

async function findNamedFile(directory, name) {
  const matches = [];
  const visit = async (current) => {
    for (const entry of await readdir(current, { withFileTypes: true })) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile() && entry.name === name) matches.push(path);
    }
  };
  await visit(directory);
  if (matches.length !== 1) {
    throw new Error(`Expected one ${name} in ${directory}, found ${matches.length}`);
  }
  return matches[0];
}

async function findArchive(artifactsDir, target) {
  const entries = await readdir(artifactsDir);
  const prefix = `leantoken-${target}.`;
  const matches = entries.filter(
    (name) => name.startsWith(prefix) && (name.endsWith(".tar.xz") || name.endsWith(".zip")),
  );
  if (matches.length !== 1) {
    throw new Error(`Expected one archive for ${target}, found ${matches.length}`);
  }
  return join(artifactsDir, matches[0]);
}

async function extractBinary(archive, binaryName, destination) {
  const extractionDir = await mkdtemp(join(tmpdir(), "leantoken-npm-extract-"));
  try {
    if (archive.endsWith(".zip")) run("unzip", ["-q", archive, "-d", extractionDir]);
    else run("tar", ["-xJf", archive, "-C", extractionDir]);
    await copyFile(await findNamedFile(extractionDir, binaryName), destination);
    await chmod(destination, 0o755);
  } finally {
    await rm(extractionDir, { recursive: true, force: true });
  }
}

async function copyPackageDocs(destination) {
  await Promise.all(
    ["LICENSE-APACHE", "LICENSE-MIT", "README.md"].map((name) =>
      copyFile(join(ROOT, name), join(destination, name)),
    ),
  );
}

async function writeJson(path, value) {
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

function commonMetadata(name, version, description) {
  return {
    name,
    version,
    description,
    license: "MIT OR Apache-2.0",
    repository: REPOSITORY,
    homepage: REPOSITORY,
    engines: { node: ">=18" },
  };
}

async function buildRootPackage(stagingDir, artifactsDir, version) {
  const packageDir = join(stagingDir, "leantoken");
  const binDir = join(packageDir, "bin");
  await mkdir(binDir, { recursive: true });
  const packageJson = {
    ...commonMetadata(
      "leantoken",
      version,
      "Token-budgeted repository context for coding agents",
    ),
    bin: { leantoken: "bin/leantoken.cjs" },
    files: ["bin", "platforms.json", "LICENSE-APACHE", "LICENSE-MIT", "README.md"],
  };
  await writeJson(join(packageDir, "package.json"), packageJson);
  await copyPackageDocs(packageDir);
  await copyFile(join(ROOT, "npm", "leantoken.cjs"), join(binDir, "leantoken.cjs"));
  await copyFile(join(ROOT, "npm", "platforms.json"), join(packageDir, "platforms.json"));
  await chmod(join(binDir, "leantoken.cjs"), 0o755);

  for (const platform of PLATFORMS) {
    const nativeDir = join(binDir, "native", platform.target);
    await mkdir(nativeDir, { recursive: true });
    await extractBinary(
      await findArchive(artifactsDir, platform.target),
      platform.binary,
      join(nativeDir, platform.binary),
    );
  }
  return packageDir;
}

function pack(packageDir, outputDir) {
  run("npm", ["pack", "--silent", "--pack-destination", outputDir, packageDir], {
    capture: true,
  });
}

export async function buildNpmPackages({ artifactsDir, outputDir, version }) {
  const resolvedArtifacts = resolve(artifactsDir);
  const resolvedOutput = resolve(outputDir);
  if (!(await stat(resolvedArtifacts)).isDirectory()) {
    throw new Error(`Artifact path is not a directory: ${resolvedArtifacts}`);
  }
  await mkdir(resolvedOutput, { recursive: true });
  const existingPackages = (await readdir(resolvedOutput)).filter((name) => name.endsWith(".tgz"));
  if (existingPackages.length > 0) {
    throw new Error(`Output directory already contains npm packages: ${resolvedOutput}`);
  }

  const stagingDir = await mkdtemp(join(tmpdir(), "leantoken-npm-packages-"));
  try {
    pack(await buildRootPackage(stagingDir, resolvedArtifacts, version), resolvedOutput);
  } finally {
    await rm(stagingDir, { recursive: true, force: true });
  }
}

export async function readCargoVersion() {
  const manifest = await readFile(join(ROOT, "Cargo.toml"), "utf8");
  const packageStart = manifest.indexOf("[package]");
  const afterPackage = packageStart === -1 ? "" : manifest.slice(packageStart + "[package]".length);
  const nextSection = afterPackage.search(/^\[/m);
  const packageSection = nextSection === -1 ? afterPackage : afterPackage.slice(0, nextSection);
  const version = /^version\s*=\s*"([^"]+)"\s*$/m.exec(packageSection ?? "")?.[1];
  if (!version) throw new Error("Could not read package version from Cargo.toml");
  return version;
}

function parseArgs(args) {
  const values = {};
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    const value = args[index + 1];
    if (!key?.startsWith("--") || !value) throw new Error(`Invalid argument: ${key ?? ""}`);
    values[key.slice(2)] = value;
  }
  if (!values.artifacts || !values.out) {
    throw new Error("Usage: build-npm-packages.mjs --artifacts <dir> --out <dir> [--version <v>]");
  }
  return values;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = parseArgs(process.argv.slice(2));
  await buildNpmPackages({
    artifactsDir: args.artifacts,
    outputDir: args.out,
    version: args.version ?? (await readCargoVersion()),
  });
}
