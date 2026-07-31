#!/usr/bin/env node
// 把根 Cargo.toml 里的仓库元数据同步到 GitHub：
//   - [workspace.package].description   → GitHub 仓库 description
//   - [workspace.metadata.github].topics → GitHub 仓库 topics
//
// 这是「唯一源」：改 Cargo.toml 一处，跑 `node scripts/sync-github-meta.mjs` 即刷到 GitHub。
// 零依赖（node 内置 fs + 针对性 TOML 解析，只取我们需要的两段，不引入 toml 包）。
// 前置：gh 已登录（gh auth login）。

import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, "..");
const TOML = resolve(ROOT, "Cargo.toml");

// ---- 针对性解析：只要 [workspace.package].description 和 [workspace.metadata.github].topics ----
function parseMeta(text) {
  // 切到 [workspace] 段开始（忽略前面的 [package] 等，防止误匹配同名键）。
  const wsStart = text.indexOf("[workspace]");
  const ws = wsStart >= 0 ? text.slice(wsStart) : text;

  // description：在 [workspace.package] 子段内，形如 description = "...."
  const pkgBlock = sliceTable(ws, "package");
  const descMatch = pkgBlock.match(/^\s*description\s*=\s*"((?:[^"\\]|\\.)*)"/m);
  const description = descMatch ? unescapeToml(descMatch[1]) : "";

  // topics：在 [workspace.metadata.github] 子段内的数组。
  const ghBlock = sliceTable(ws, "metadata.github");
  const topics = parseTomlArray(ghBlock);

  return { description, topics };
}

// 取 `name` 子表的内容（到下一个 `[` 开头的表或文件尾）。
function sliceTable(text, dottedName) {
  // 匹配 [workspace.package] 或 [workspace.metadata.github] 这类带点名的表头。
  const re = new RegExp(`^\\[workspace\\.${dottedName.replace(".", "\\.")}\\]\\s*\\n`, "m");
  const m = text.match(re);
  if (!m) return "";
  const start = m.index + m[0].length;
  // 下一个顶层表头 [xxx]（非 [xxx.yyy] 也行，但必须是新表）即结束。
  const rest = text.slice(start);
  const end = rest.search(/^\s*\[[^\s]/m);
  return end >= 0 ? rest.slice(0, end) : rest;
}

function parseTomlArray(block) {
  // 形如 topics = [ "a", "b", ] 或跨行。先化简：抓最外层 [ ... ]。
  const m = block.match(/topics\s*=\s*\[([\s\S]*?)\]/);
  if (!m) return [];
  return [...m[1].matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((x) => unescapeToml(x[1]));
}

function unescapeToml(s) {
  return s.replace(/\\n/g, "\n").replace(/\\t/g, "\t").replace(/\\"/g, '"').replace(/\\\\/g, "\\");
}

// ---- 主流程 ----
function run() {
  const text = readFileSync(TOML, "utf8");
  const { description, topics } = parseMeta(text);

  if (!description) {
    console.error("error: Cargo.toml 里找不到 [workspace.package].description");
    process.exit(1);
  }
  if (topics.length === 0) {
    console.error("error: Cargo.toml 里找不到 [workspace.metadata.github].topics");
    process.exit(1);
  }

  try {
    execFileSync("gh", ["auth", "status"], { stdio: "ignore" });
  } catch {
    console.error("error: gh 未登录，先 `gh auth login`");
    process.exit(1);
  }

  const slug = execFileSync("gh", ["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"], {
    encoding: "utf8",
  }).trim();

  console.log(`仓库: ${slug}`);
  console.log(`description: ${description}`);
  console.log(`topics: ${topics.join(", ")}`);

  // description 设置。
  execFileSync("gh", ["repo", "edit", slug, "--description", description], { stdio: "inherit" });

  // topics 一次 PATCH 整体覆盖（GitHub topics 为全量替换语义）。
  const topicsJson = JSON.stringify(topics);
  execFileSync(
    "gh",
    ["api", "-X", "PATCH", `/repos/${slug}`, "-f", `description=${description}`, "-f", `topics=${topicsJson}`],
    { stdio: "inherit" }
  );

  console.log("OK: GitHub description 与 topics 已同步（来源 = 根 Cargo.toml）");
}

run();
