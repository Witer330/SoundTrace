#!/usr/bin/env node
/**
 * 把转写引擎（子进程）构建到 Tauri 期望的位置。
 *
 * 为什么需要这一层：原生识别引擎在特定音频内容上会破坏栈保护并让进程直接退出，
 * 因此转写被放进独立子进程（src/bin/st-transcribe.rs），由主程序 spawn。
 * Tauri 打包要求外部二进制带目标三元组后缀并放在 src-tauri/binaries/ 下，
 * 这个脚本负责构建并放到那个位置，保证 `pnpm tauri build` 打出的安装包里带上引擎。
 *
 * 开发时不需要它：`cargo build` 会把 bin 与主程序放在同一 profile 目录，主程序按同级查找即可。
 */
import { execSync } from "node:child_process";
import { mkdirSync, copyFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = dirname(here); // 仓库根
const srcTauri = join(root, "src-tauri");
const binariesDir = join(srcTauri, "binaries");

function run(cmd, cwd) {
  execSync(cmd, { cwd, stdio: "inherit" });
}

// 目标三元组：取 rustc 报的 host（未显式指定 --target 时 cargo 就用它）
const hostLine = execSync("rustc -vV", { encoding: "utf8" })
  .split("\n")
  .find((l) => l.startsWith("host: "));
if (!hostLine) {
  console.error("[sidecar] 无法从 rustc -vV 解析 host 三元组");
  process.exit(1);
}
const triple = hostLine.slice("host: ".length).trim();

const srcName = process.platform === "win32" ? "st-transcribe.exe" : "st-transcribe";
const isWin = process.platform === "win32";
const tripleName = `${srcName.replace(/\.exe$/, "")}-${triple}${isWin ? ".exe" : ""}`;

console.log(`[sidecar] 构建转写引擎（${triple}）…`);
run("cargo build --release --bin st-transcribe", srcTauri);

const built = join(srcTauri, "target", "release", srcName);
if (!existsSync(built)) {
  console.error(`[sidecar] 构建产物不存在：${built}`);
  process.exit(1);
}

mkdirSync(binariesDir, { recursive: true });
// 产物放两份：
// - 带三元组名：Tauri 的 externalBin 约定 / 便于人工辨认目标平台
// - 固定名：tauri.conf.json 的 bundle.resources 引用它（路径需跨平台稳定）
for (const name of [tripleName, srcName]) {
  const dest = join(binariesDir, name);
  copyFileSync(built, dest);
  console.log(`[sidecar] 已就位：${dest}`);
}
