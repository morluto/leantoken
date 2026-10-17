import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { PLATFORMS } from "../scripts/build-npm-packages.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function run(program, args, options = {}) {
  if (program === "npm" && process.platform === "win32") {
    program = process.env.ComSpec ?? "cmd.exe";
    args = ["/d", "/c", "npm.cmd", ...args];
  }
  const result = spawnSync(program, args, {
    encoding: "utf8",
    ...options,
  });
  assert.ifError(result.error);
  assert.equal(
    result.status,
    0,
    `${program} ${args.join(" ")} failed:\n${result.stderr || result.stdout}`,
  );
  return result;
}

test("installs and runs the host-native npm package without lifecycle scripts", async () => {
  const platform = PLATFORMS.find(
    ({ os, cpu }) => os === process.platform && cpu === process.arch,
  );
  assert.ok(platform, `No npm target for ${process.platform}-${process.arch}`);

  const workspace = await mkdtemp(join(tmpdir(), "leantoken-npm-install-e2e-"));
  const packageDir = join(workspace, "package");
  const nativeDir = join(packageDir, "bin", "native", platform.target);
  const outputDir = join(workspace, "output");
  const installDir = join(workspace, "install");
  const npmCache = join(workspace, "npm-cache");
  const env = {
    ...process.env,
    NO_UPDATE_NOTIFIER: "1",
    NPM_CONFIG_CACHE: npmCache,
    NPM_CONFIG_UPDATE_NOTIFIER: "false",
  };
  delete env.CARGO_BUILD_TARGET;

  try {
    const metadata = JSON.parse(
      run("cargo", ["metadata", "--no-deps", "--locked", "--format-version", "1"], {
        cwd: ROOT,
        env,
      }).stdout,
    );
    const manifest = resolve(ROOT, "Cargo.toml");
    const product = metadata.packages.find(
      ({ name, manifest_path }) => name === "leantoken" && resolve(manifest_path) === manifest,
    );
    assert.ok(product, `Cargo metadata has no product package at ${manifest}`);
    const { version } = product;
    run("cargo", ["build", "--locked", "-p", "leantoken", "--bin", "leantoken"], {
      cwd: ROOT,
      env,
    });
    const productBinary = join(metadata.target_directory, "debug", platform.binary);
    await mkdir(nativeDir, { recursive: true });
    await mkdir(outputDir);
    await mkdir(installDir);
    await copyFile(productBinary, join(nativeDir, platform.binary));
    if (process.platform !== "win32") {
      await chmod(join(nativeDir, platform.binary), 0o755);
    }
    await copyFile(join(ROOT, "npm", "leantoken.cjs"), join(packageDir, "bin", "leantoken.cjs"));
    await copyFile(join(ROOT, "npm", "platforms.json"), join(packageDir, "platforms.json"));
    await writeFile(
      join(packageDir, "package.json"),
      `${JSON.stringify({
        name: "leantoken",
        version,
        engines: { node: ">=18" },
        bin: { leantoken: "bin/leantoken.cjs" },
        files: ["bin", "platforms.json"],
      })}\n`,
    );

    run("npm", ["pack", "--offline", "--silent", "--pack-destination", outputDir, packageDir], {
      env,
    });
    const tarball = join(outputDir, `leantoken-${version}.tgz`);
    await writeFile(
      join(installDir, "package.json"),
      `${JSON.stringify({
        private: true,
        dependencies: { leantoken: `file:${tarball}` },
      })}\n`,
    );

    const install = run(
      "npm",
      ["install", "--ignore-scripts", "--offline", "--no-audit", "--no-fund"],
      { cwd: installDir, env },
    );
    assert.doesNotMatch(install.stderr, /allow-scripts|lifecycle script|postinstall/i);

    const cli = ["exec", "--offline", "--", "leantoken"];
    const versionOutput = run("npm", [...cli, "--version"], { cwd: installDir, env });
    assert.ok(versionOutput.stdout.includes(version), "launcher did not report Cargo version");

    const repository = join(workspace, "repository with spaces");
    const database = join(workspace, "explicit index.sqlite3");
    const repositoryOptions = ["--root", repository, "--database", database, "--json"];
    await mkdir(join(repository, "src"), { recursive: true });
    await writeFile(
      join(repository, "src", "npm_install_fixture.rs"),
      "pub fn npm_install_e2e_unique_marker() -> &'static str { \"installed launcher\" }\n",
    );
    run("npm", [...cli, ...repositoryOptions, "index"], { cwd: installDir, env });
    const status = JSON.parse(
      run(
        "npm",
        [...cli, ...repositoryOptions, "status"],
        { cwd: installDir, env },
      ).stdout,
    );
    assert.equal(status.file_count, 1);

    const search = JSON.parse(
      run(
        "npm",
        [
          ...cli,
          ...repositoryOptions,
          "search",
          "npm_install_e2e_unique_marker",
          "--mode",
          "identifier",
          "--max-tokens",
          "100",
        ],
        { cwd: installDir, env },
      ).stdout,
    );
    assert.equal(search.hits[0].path, "src/npm_install_fixture.rs");
    assert.ok(search.meta.source_tokens <= 100, "search exceeded --max-tokens");
  } finally {
    await rm(workspace, { recursive: true, force: true });
  }
});
