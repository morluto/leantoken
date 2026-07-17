#!/usr/bin/env node

const { spawn } = require("node:child_process");
const { join } = require("node:path");
const platforms = require("../platforms.json");

const libc =
  process.platform === "linux"
    ? process.report?.getReport?.().header?.glibcVersionRuntime
      ? "glibc"
      : "musl"
    : undefined;
const target = platforms.find(
  ({ os, cpu, libc: requiredLibc }) =>
    os === process.platform &&
    cpu === process.arch &&
    (requiredLibc === undefined || requiredLibc === libc),
);
if (!target) {
  const runtime = [process.platform, process.arch, libc].filter(Boolean).join("-");
  console.error(
    `LeanToken does not provide an npm binary for ${runtime}.`,
  );
  process.exit(1);
}

const binaryPath = join(__dirname, "native", target.target, target.binary);

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
