#!/usr/bin/env node

const { spawn } = require("node:child_process");
const platforms = require("../platforms.json");

const target = platforms.find(
  ({ os, cpu }) => os === process.platform && cpu === process.arch,
);
if (!target) {
  console.error(
    `LeanToken does not provide an npm binary for ${process.platform}-${process.arch}.`,
  );
  process.exit(1);
}

const { packageName, binary: binaryName } = target;
let binaryPath;
try {
  binaryPath = require.resolve(`${packageName}/${binaryName}`);
} catch {
  console.error(`LeanToken native package ${packageName} is missing.`);
  console.error(
    "Reinstall leantoken without omitting optional dependencies, or use a Cargo installation.",
  );
  process.exit(1);
}

const child = spawn(binaryPath, process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: true,
});

const signals =
  process.platform === "win32" ? ["SIGINT", "SIGTERM"] : ["SIGHUP", "SIGINT", "SIGTERM"];
const signalHandlers = new Map(
  signals.map((signal) => [
    signal,
    () => {
      if (!child.killed) child.kill(signal);
    },
  ]),
);
for (const [signal, handler] of signalHandlers) process.on(signal, handler);

child.once("error", (error) => {
  console.error(`Failed to start LeanToken: ${error.message}`);
  process.exitCode = 1;
});

child.once("close", (code, signal) => {
  for (const [forwarded, handler] of signalHandlers) process.removeListener(forwarded, handler);

  if (signal && process.platform !== "win32") {
    process.kill(process.pid, signal);
    return;
  }
  process.exitCode = code ?? 1;
});
