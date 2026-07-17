import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import test from "node:test";

import { PLATFORMS, buildNpmPackages, readCargoVersion } from "../scripts/build-npm-packages.mjs";

function run(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: "utf8", ...options });
  assert.equal(
    result.status,
    0,
    `${program} ${args.join(" ")} failed:\n${result.stderr || result.stdout}`,
  );
  return result.stdout.trim();
}

async function makeArchive(artifactsDir, platform) {
  const source = await mkdtemp(join(tmpdir(), "leantoken-npm-fixture-"));
  try {
    const binary = join(source, platform.binary);
    await writeFile(binary, '#!/bin/sh\nprintf "fake-leantoken:%s\\n" "$*"\n');
    await chmod(binary, 0o755);

    if (platform.target.endsWith("windows-msvc")) {
      const archive = join(artifactsDir, `leantoken-${platform.target}.zip`);
      run("python3", ["-m", "zipfile", "-c", archive, platform.binary], { cwd: source });
      return;
    }

    const root = join(source, `leantoken-${platform.target}`);
    await mkdir(root);
    await writeFile(join(root, platform.binary), await readFile(binary));
    await chmod(join(root, platform.binary), 0o755);
    run("tar", [
      "-cJf",
      join(artifactsDir, `leantoken-${platform.target}.tar.xz`),
      "-C",
      source,
      basename(root),
    ]);
  } finally {
    await rm(source, { recursive: true, force: true });
  }
}

async function unpackPackage(tarball, workspace) {
  const directory = await mkdtemp(join(workspace, "unpack-"));
  run("tar", ["-xzf", tarball, "-C", directory]);
  return directory;
}

test("reads the npm package version from Cargo.toml", async () => {
  assert.match(await readCargoVersion(), /^\d+\.\d+\.\d+/);
});

test("builds one script-free package containing every native binary", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "leantoken-npm-test-"));
  const artifacts = join(workspace, "artifacts");
  const output = join(workspace, "packages");
  const version = "9.8.7";
  await mkdir(artifacts);

  try {
    for (const platform of PLATFORMS) await makeArchive(artifacts, platform);
    await buildNpmPackages({ artifactsDir: artifacts, outputDir: output, version });

    const tarballs = (await readdir(output)).sort();
    assert.deepEqual(tarballs, [`leantoken-${version}.tgz`]);

    const rootTarball = join(output, `leantoken-${version}.tgz`);
    const root = await unpackPackage(rootTarball, workspace);
    const rootPackage = JSON.parse(await readFile(join(root, "package", "package.json")));
    assert.equal(rootPackage.scripts, undefined);
    assert.equal(rootPackage.optionalDependencies, undefined);

    for (const platform of PLATFORMS) {
      const binary = await stat(
        join(root, "package", "bin", "native", platform.target, platform.binary),
      );
      assert.equal(binary.isFile(), true);
      assert.notEqual(binary.mode & 0o111, 0);
    }

    if (process.platform === "linux" && process.arch === "x64") {
      const install = join(workspace, "install");
      await mkdir(install);
      await writeFile(
        join(install, "package.json"),
        `${JSON.stringify({
          private: true,
          dependencies: {
            leantoken: `file:${rootTarball}`,
          },
        })}\n`,
      );
      run(
        "npm",
        [
          "install",
          "--ignore-scripts",
          "--offline",
          "--no-audit",
          "--no-fund",
        ],
        { cwd: install },
      );
      assert.equal(
        run(join(install, "node_modules", ".bin", "leantoken"), ["status", "check"]),
        "fake-leantoken:status check",
      );

      const launcher = join(
        install,
        "node_modules",
        "leantoken",
        "bin",
        "leantoken.cjs",
      );
      const musl = spawnSync(
        process.execPath,
        [
          "-e",
          `process.report.getReport = () => ({ header: {} }); require(${JSON.stringify(launcher)});`,
        ],
        { encoding: "utf8" },
      );
      assert.equal(musl.status, 1);
      assert.match(musl.stderr, /does not provide an npm binary for linux-x64-musl/);
    }
  } finally {
    await rm(workspace, { recursive: true, force: true });
  }
});
