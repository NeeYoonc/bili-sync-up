"use strict";
// TikTok webmssdk SDK 管理器：发现候选版本、结构测试、激活/回滚。
// 用法:
//   node tiktok-sdk-manager.cjs discover          # 从官方页面发现并下载候选
//   node tiktok-sdk-manager.cjs test <runtime_id> # 结构测试（用候选签名测试 URL）
//   node tiktok-sdk-manager.cjs activate <runtime_id>
//   node tiktok-sdk-manager.cjs rollback
//   node tiktok-sdk-manager.cjs status            # 输出 active/previous/candidates JSON
// 依赖 Node.js（>=18，全局 fetch）。目录结构：
//   CONFIG_DIR/tiktok-sdk/{active.json, previous.json, candidates/<id>/{webmssdk.js, manifest.json}}
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");

const CFG_DIR = process.env.BILI_SYNC_CONFIG_DIR || "C:/Users/001/AppData/Roaming/bili-sync";
const SDK_DIR = path.join(CFG_DIR, "tiktok-sdk");
const CANDIDATES_DIR = path.join(SDK_DIR, "candidates");
const ACTIVE_FILE = path.join(SDK_DIR, "active.json");
const PREVIOUS_FILE = path.join(SDK_DIR, "previous.json");
const SIGNER = path.join(__dirname, "tiktok-signer.cjs");

const UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";
const DISCOVERY_PAGES = Object.freeze([
  "https://www.tiktok.com/",
  "https://www.tiktok.com/explore",
  "https://www.tiktok.com/login",
  "https://www.tiktok.com/foryou",
]);
const BOOTSTRAP_SDK_URLS = Object.freeze([
  "https://sf16-website-login.neutral.ttwstatic.com/obj/tiktok_web_login_static/webmssdk/1.0.0.388/webmssdk.js",
]);
// 结构测试使用的关注列表请求（与真实签名链路一致）。
const TEST_URL = "https://www.tiktok.com/api/user/list/?aid=1988&app_name=tiktok_web&scene=21&count=1&maxCursor=0&minCursor=0";
const MAX_SDK_BYTES = 5_000_000;

function ensureDirs() {
  fs.mkdirSync(CANDIDATES_DIR, { recursive: true });
}
function atomicJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const tmp = path.join(path.dirname(file), "." + path.basename(file) + "." + crypto.randomUUID());
  fs.writeFileSync(tmp, JSON.stringify(value, null, 2) + "\n");
  fs.renameSync(tmp, file);
}
function readJson(file) {
  try { return JSON.parse(fs.readFileSync(file, "utf8")); } catch { return null; }
}
function listCandidateManifests() {
  const values = [];
  if (!fs.existsSync(CANDIDATES_DIR)) return values;
  for (const name of fs.readdirSync(CANDIDATES_DIR)) {
    const m = readJson(path.join(CANDIDATES_DIR, name, "manifest.json"));
    if (m) values.push(m);
  }
  return values;
}
function validId(value) {
  return /^[A-Za-z0-9_.-]{1,96}$/.test(String(value));
}
function sdkUrlDetails(value) {
  try {
    const url = new URL(String(value));
    if (url.protocol !== "https:") return null;
    const m = url.pathname.match(/\/webmssdk\/(\d+(?:\.\d+)+)\/webmssdk\.js$/);
    if (!m) return null;
    url.hash = "";
    return { url: url.href, release: m[1] };
  } catch { return null; }
}
function extractSdkUrls(html) {
  const normalized = String(html)
    .replace(/\\u002[fF]/g, "/")
    .replace(/\\x2[fF]/g, "/")
    .replace(/\\\//g, "/");
  const matches = normalized.matchAll(/https:\/\/[^"'<>\s]+\/webmssdk\/\d+(?:\.\d+)+\/webmssdk\.js(?:\?[^"'<>\s]*)?/g);
  const values = [];
  const seen = new Set();
  for (const m of matches) {
    const d = sdkUrlDetails(m[0]);
    if (!d || seen.has(d.url)) continue;
    seen.add(d.url);
    values.push(d.url);
  }
  return values;
}
function versions(source) {
  const scm = String(source).match(/\.scmVersion="([^"]+)"/)?.[1] ?? "unknown";
  const sdk = String(source).match(/\.sdkVersion="([^"]+)"/)?.[1] ?? "unknown";
  return { scm_version: scm, sdk_version: sdk };
}
async function fetchBytes(url, headers, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(url, { headers, signal: controller.signal, redirect: "follow" });
    if (!res.ok) throw new Error("HTTP " + res.status + " for " + url);
    const buf = Buffer.from(await res.arrayBuffer());
    if (buf.length > MAX_SDK_BYTES) throw new Error("SDK candidate exceeds size limit");
    return buf;
  } finally { clearTimeout(timer); }
}
async function discover() {
  ensureDirs();
  const pageHeaders = {
    "user-agent": UA,
    accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    "accept-language": "en-US,en;q=0.9",
  };
  const sdkHeaders = { "user-agent": UA, accept: "*/*", referer: "https://www.tiktok.com/" };
  const urls = new Set(BOOTSTRAP_SDK_URLS);
  // 已持久化的 SDK 地址也加入发现集合：页面抓取失败时仍可复验/续期已知版本。
  for (const value of [readJson(ACTIVE_FILE), readJson(PREVIOUS_FILE), ...listCandidateManifests()]) {
    const details = value && sdkUrlDetails(value.sdk_url);
    if (details) urls.add(details.url);
  }
  for (const page of DISCOVERY_PAGES) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 15000);
    try {
      const res = await fetch(page, { headers: pageHeaders, signal: controller.signal, redirect: "follow" });
      if (res.ok) {
        const html = await res.text();
        extractSdkUrls(html).forEach((u) => urls.add(u));
      }
    } catch { /* 页面不可达时跳过 */ }
    finally { clearTimeout(timer); }
  }
  const results = [];
  for (const url of urls) {
    const details = sdkUrlDetails(url);
    if (!details) continue;
    try {
      const buf = await fetchBytes(details.url, sdkHeaders, 20000);
      const digest = crypto.createHash("sha256").update(buf).digest("hex");
      const runtimeId = "webmssdk-" + digest.slice(0, 16);
      const dir = path.join(CANDIDATES_DIR, runtimeId);
      const sdkPath = path.join(dir, "webmssdk.js");
      if (!fs.existsSync(sdkPath)) {
        fs.mkdirSync(dir, { recursive: true });
        fs.writeFileSync(sdkPath, buf);
      }
      const v = versions(buf.toString("utf8"));
      const manifestPath = path.join(dir, "manifest.json");
      const existing = readJson(manifestPath) || {};
      atomicJson(manifestPath, {
        runtime_id: runtimeId,
        source: "official-webmssdk",
        sdk_url: details.url,
        sdk_sha256: digest,
        scm_version: v.scm_version,
        sdk_version: v.sdk_version,
        status: existing.status || "candidate",
        discovered_at: existing.discovered_at || new Date().toISOString(),
      });
      results.push(runtimeId);
    } catch (e) {
      console.error("[discover] failed for " + url + ": " + e.message);
    }
  }
  console.log("DISCOVERED " + results.length);
  for (const id of results) console.log(id);
}
function structuralTest(runtimeId) {
  if (!validId(runtimeId)) { console.error("invalid runtime id"); process.exit(1); }
  const manifestPath = path.join(CANDIDATES_DIR, runtimeId, "manifest.json");
  const manifest = readJson(manifestPath);
  if (!manifest) { console.error("ERROR: candidate not found: " + runtimeId); process.exit(1); }
  const res = spawnSync(process.execPath, [SIGNER, TEST_URL, "--sdk", runtimeId], {
    encoding: "utf8", timeout: 90000,
  });
  const stdout = String(res.stdout || "");
  const signed = stdout.split("\n").map((l) => l.trim()).filter((l) => l.startsWith("https://")).pop() || "";
  const sizes = {
    "X-Dynosaur": ((signed.match(/X-Dynosaur=([^&\s]+)/) || [])[1] || "").length,
    "X-Gnarly": ((signed.match(/X-Gnarly=([^&\s]+)/) || [])[1] || "").length,
    "X-Bogus": ((signed.match(/X-Bogus=([^&\s]+)/) || [])[1] || "").length,
  };
  if (sizes["X-Dynosaur"] < 100 || sizes["X-Gnarly"] < 100 || sizes["X-Bogus"] < 1) {
    manifest.status = "test_failed";
    manifest.test_error = "signature sizes " + JSON.stringify(sizes) + " stderr=" + String(res.stderr || "").slice(0, 200);
    atomicJson(manifestPath, manifest);
    console.log("FAIL " + runtimeId + " " + JSON.stringify(sizes));
    process.exit(1);
  }
  manifest.status = manifest.status === "remote_verified" ? "remote_verified" : "tested";
  manifest.tested_at = new Date().toISOString();
  delete manifest.test_error;
  atomicJson(manifestPath, manifest);
  console.log("OK " + runtimeId + " " + JSON.stringify(sizes));
}
function markRemoteVerified(runtimeId, userListCount) {
  if (!validId(runtimeId)) { console.error("invalid runtime id"); process.exit(1); }
  const manifestPath = path.join(CANDIDATES_DIR, runtimeId, "manifest.json");
  const manifest = readJson(manifestPath);
  if (!manifest) { console.error("ERROR: candidate not found: " + runtimeId); process.exit(1); }
  if (manifest.status !== "tested" && manifest.status !== "remote_verified") {
    console.error("ERROR: candidate must pass structural test first (status=" + manifest.status + "): " + runtimeId);
    process.exit(1);
  }
  const count = Number(userListCount);
  if (!Number.isInteger(count) || count <= 0) {
    console.error("ERROR: invalid user_list_count: " + userListCount);
    process.exit(1);
  }
  manifest.status = "remote_verified";
  manifest.verified_at = new Date().toISOString();
  manifest.verification = { following_json_valid: true, status_code: 0, user_list_count: count };
  atomicJson(manifestPath, manifest);
  console.log("VERIFIED " + runtimeId + " " + count);
}
function activate(runtimeId) {
  if (!validId(runtimeId)) { console.error("invalid runtime id"); process.exit(1); }
  const manifest = readJson(path.join(CANDIDATES_DIR, runtimeId, "manifest.json"));
  if (!manifest) { console.error("ERROR: candidate not found: " + runtimeId); process.exit(1); }
  if (manifest.status !== "remote_verified") {
    console.error("ERROR: candidate must be remote-verified before activation (status=" + manifest.status + "): " + runtimeId);
    process.exit(1);
  }
  const current = readJson(ACTIVE_FILE);
  if (current && current.runtime_id === runtimeId) {
    // 已激活同版本：仅刷新元数据，不覆盖 previous（否则回滚会退化为空操作）。
    atomicJson(ACTIVE_FILE, { ...current, ...manifest, activated_at: current.activated_at ?? new Date().toISOString() });
    console.log("ACTIVATED " + runtimeId);
    return;
  }
  atomicJson(PREVIOUS_FILE, current);
  atomicJson(ACTIVE_FILE, { ...manifest, activated_at: new Date().toISOString() });
  console.log("ACTIVATED " + runtimeId);
}
function rollback() {
  const previous = readJson(PREVIOUS_FILE);
  if (!previous) { console.error("ERROR: no previous runtime exists"); process.exit(1); }
  const current = readJson(ACTIVE_FILE);
  atomicJson(ACTIVE_FILE, { ...previous, rolled_back_at: new Date().toISOString() });
  atomicJson(PREVIOUS_FILE, current);
  console.log("ROLLED_BACK " + String(previous.runtime_id || "?"));
}
function status() {
  ensureDirs();
  const active = readJson(ACTIVE_FILE);
  const previous = readJson(PREVIOUS_FILE);
  const candidates = [];
  if (fs.existsSync(CANDIDATES_DIR)) {
    for (const name of fs.readdirSync(CANDIDATES_DIR)) {
      const m = readJson(path.join(CANDIDATES_DIR, name, "manifest.json"));
      if (m) {
        candidates.push({
          runtime_id: name,
          scm_version: m.scm_version,
          sdk_version: m.sdk_version,
          status: m.status,
          sdk_sha256: m.sdk_sha256,
        });
      }
    }
  }
  console.log(JSON.stringify({ active, previous, candidates }, null, 2));
}
async function main() {
  const cmd = process.argv[2];
  if (cmd === "discover") return discover();
  if (cmd === "test") return structuralTest(process.argv[3]);
  if (cmd === "verify") return markRemoteVerified(process.argv[3], process.argv[4]);
  if (cmd === "activate") return activate(process.argv[3]);
  if (cmd === "rollback") return rollback();
  if (cmd === "status") return status();
  console.error("usage: node tiktok-sdk-manager.cjs <discover|test|verify|activate|rollback|status> ...");
  process.exit(1);
}
main().catch((e) => { console.error("ERROR: " + e.message); process.exit(1); });
