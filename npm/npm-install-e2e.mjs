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

function runNpm(args, options = {}) {
  if (process.platform === "win32") {
    return run(process.env.ComSpec ?? "cmd.exe", ["/d", "/c", "npm.cmd", ...args], options);
  }
  return run("npm", args, options);
}

test("installs and runs the host-native npm package without lifecycle scripts", async () => {
  const platform = PLATFORMS.find(
    ({ os, cpu }) => os === process.platform && cpu === process.arch,
  );
  assert.ok(platform, `No npm target for ${process.platform}-${process.arch}`);

  const version = "9.8.7";
  const workspace = await mkdtemp(join(tmpdir(), "leantoken-npm-install-e2e-"));
  const packageDir = join(workspace, "package");
  const nativeDir = join(packageDir, "bin", "native", platform.target);
  const fixtureSource = join(workspace, "fixture.rs");
  const fixtureBinary = join(workspace, platform.binary);
  const outputDir = join(workspace, "output");
  const installDir = join(workspace, "install");
  const npmCache = join(workspace, "npm-cache");
  const env = {
    ...process.env,
    NO_UPDATE_NOTIFIER: "1",
    NPM_CONFIG_CACHE: npmCache,
    NPM_CONFIG_UPDATE_NOTIFIER: "false",
  };

  try {
    await writeFile(
      fixtureSource,
      'fn main() { println!("fake-leantoken:{:?}", std::env::args().skip(1).collect::<Vec<_>>()); }\n',
    );
    run("rustc", [fixtureSource, "-o", fixtureBinary]);
    await mkdir(nativeDir, { recursive: true });
    await mkdir(outputDir);
    await mkdir(installDir);
    await copyFile(fixtureBinary, join(nativeDir, platform.binary));
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

    runNpm(["pack", "--silent", "--pack-destination", outputDir, packageDir], { env });
    const tarball = join(outputDir, `leantoken-${version}.tgz`);
    await writeFile(
      join(installDir, "package.json"),
      `${JSON.stringify({
        private: true,
        dependencies: { leantoken: `file:${tarball}` },
      })}\n`,
    );

    const install = runNpm(
      ["install", "--ignore-scripts", "--offline", "--no-audit", "--no-fund"],
      { cwd: installDir, env },
    );
    assert.doesNotMatch(install.stderr, /allow-scripts|lifecycle script|postinstall/i);

    const execution = runNpm(
      ["exec", "--offline", "--", "leantoken", "status", "two words", "--flag=value"],
      { cwd: installDir, env },
    );
    assert.equal(
      execution.stdout.trim(),
      'fake-leantoken:["status", "two words", "--flag=value"]',
    );
  } finally {
    await rm(workspace, { recursive: true, force: true });
  }
});
