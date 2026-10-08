import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import test from "node:test";
import { once } from "node:events";

import { PLATFORMS, buildNpmPackages } from "../scripts/build-npm-packages.mjs";

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

async function cargoPackageVersion() {
  const manifest = await readFile(new URL("../Cargo.toml", import.meta.url), "utf8");
  const packageSection = manifest.split(/^\[package\]\s*$/m)[1]?.split(/^\[/m)[0] ?? "";
  const version = packageSection.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  assert.ok(version, "Cargo.toml must declare a package version");
  return version;
}

test("keeps cargo-dist targets aligned with the canonical npm platform manifest", async () => {
  const distWorkspace = await readFile(new URL("../dist-workspace.toml", import.meta.url), "utf8");
  const targetLine = distWorkspace.match(/^targets = (\[[^\n]+\])$/m);
  assert.ok(targetLine, "dist-workspace.toml must declare its release targets");

  const distTargets = JSON.parse(targetLine[1]);
  const npmTargets = PLATFORMS.map((platform) => platform.target);
  assert.equal(new Set(npmTargets).size, npmTargets.length, "npm targets must be unique");
  assert.deepEqual([...distTargets].sort(), [...npmTargets].sort());
});

test("uses the Cargo package version when the release CLI omits --version", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "leantoken-npm-version-"));
  const artifacts = join(workspace, "artifacts");
  const output = join(workspace, "packages");
  await mkdir(artifacts);

  try {
    for (const platform of PLATFORMS) await makeArchive(artifacts, platform);
    run(process.execPath, [
      new URL("../scripts/build-npm-packages.mjs", import.meta.url).pathname,
      "--artifacts",
      artifacts,
      "--out",
      output,
    ]);

    assert.deepEqual(
      await readdir(output),
      [`leantoken-${await cargoPackageVersion()}.tgz`],
    );
  } finally {
    await rm(workspace, { recursive: true, force: true });
  }
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

test("forwards a later termination signal while the native child remains alive", {
  skip: process.platform === "win32",
  timeout: 10_000,
}, async (t) => {
  const workspace = await mkdtemp(join(tmpdir(), "leantoken-npm-signals-"));
  t.after(() => rm(workspace, { recursive: true, force: true }));
  const libc = process.platform === "linux"
    ? process.report?.getReport?.().header?.glibcVersionRuntime ? "glibc" : "musl"
    : undefined;
  const platform = PLATFORMS.find(({ os, cpu, libc: requiredLibc }) =>
    os === process.platform && cpu === process.arch &&
    (requiredLibc === undefined || requiredLibc === libc)
  );
  assert.ok(platform, "host platform must be present in the npm manifest");
  const native = join(workspace, "bin", "native", platform.target, platform.binary);
  await mkdir(join(workspace, "bin", "native", platform.target), { recursive: true });
  await writeFile(native, [
    `#!${process.execPath}`,
    'process.on("SIGINT", () => console.log("first-signal"));',
    'process.on("SIGTERM", () => process.exit(0));',
    'console.log("ready:" + process.pid);',
    'setInterval(() => {}, 1000);',
    "",
  ].join("\n"), { mode: 0o755 });
  const launcher = join(workspace, "bin", "leantoken.cjs");
  await writeFile(launcher, await readFile(new URL("./leantoken.cjs", import.meta.url)));
  await writeFile(join(workspace, "platforms.json"), JSON.stringify(PLATFORMS));

  // The direct child is the control: the fixture handles both signals itself.
  for (const command of [[native], [launcher]]) {
    const child = spawn(process.execPath, command, { stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    let nativePid;
    child.stdout.on("data", chunk => {
      output += chunk;
      const match = /ready:(\d+)/.exec(output);
      if (match) nativePid = Number(match[1]);
    });
    const closed = once(child, "close");
    async function waitFor(value) {
      const deadline = Date.now() + 2_000;
      while (!output.includes(value) && Date.now() < deadline) {
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      assert.ok(output.includes(value), `missing ${value}: ${output}`);
    }
    try {
      await waitFor("ready:");
      child.kill("SIGINT");
      await waitFor("first-signal");
      child.kill("SIGTERM");
      const exit = await Promise.race([
        closed,
        new Promise(resolve => setTimeout(() => resolve(null), 2_000)),
      ]);
      assert.deepEqual(exit, [0, null], "SIGTERM must reach the still-running native child");
    } finally {
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
      if (nativePid) {
        try { process.kill(nativePid, "SIGKILL"); } catch (error) {
          if (error.code !== "ESRCH") throw error;
        }
      }
      await closed;
    }
  }
});
