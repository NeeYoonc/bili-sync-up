#!/usr/bin/env node
/**
 * 哔哩哔哩漫画（manga.bilibili.com）请求签名 / 加解密 sidecar。
 *
 * PC 站内容接口现状（2026-09 站点构建）：
 *   - 请求体 {"<字段>": <值>, "m2": "<wasm 生成的密文>"}
 *   - 响应体 {"code":0,"data":null,"bytesData":"<密文>"}
 *   - 必需请求头 x-bili-data-sn（站点构建产物中的硬编码常量）
 *   - ImageToken 额外需要 m1（客户端 ECDH P-256 公钥 raw 点的 base64）
 *
 * 用 Node + 官方 Go wasm 胶水直接运行站点 wasm，不依赖浏览器。
 *
 * 用法（一次性）：
 *   node manga-signer.cjs bootstrap [--force]
 *   node manga-signer.cjs comic-detail '{"comic_id":25969}'
 *   node manga-signer.cjs image-index  '{"ep_id":934687}'
 *   node manga-signer.cjs image-token  '{"ep_id":934687}'
 *   node manga-signer.cjs chapter-pages '{"ep_id":934687}'
 *   node manga-signer.cjs download     '{"url":"https://...","out":"C:/tmp/1.jpg"}'
 *   node manga-signer.cjs search       '{"keyword":"碧蓝之海"}'
 *
 * 用法（常驻，NDJSON over stdin/stdout，供 Rust 复用同一个 wasm 运行时）：
 *   node manga-signer.cjs serve
 *   > {"id":1,"cmd":"chapter-pages","args":{"ep_id":934687}}
 *   < {"id":1,"ok":true,"result":{...}}
 *   > {"cmd":"shutdown"}
 *
 * 环境变量：
 *   MANGA_COOKIE  B站 Cookie（SESSDATA / bili_jct / buvid3 / buvid4 / DedeUserID）
 *   MANGA_BUVID3  buvid3 值（解密响应时使用）
 *   MANGA_CACHE   缓存目录，默认 ~/.bili-sync/manga
 *   MANGA_SN      跳过 sn 探测，直接使用给定常量
 *   MANGA_PROXY   HTTP 代理（http://host:port），可选
 */

"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const http = require("http");
const https = require("https");
const tls = require("tls");
const crypto = require("crypto");
const readline = require("readline");

const MANGA_ORIGIN = "https://manga.bilibili.com";
const STATIC_BASE = "https://s1.hdslb.com/bfs/manga-static/manga-pc/";
const WASM_EXEC_VERSION = "go1.23.6";
const CANONICAL_QUERY = "device=pc&platform=web&nov=27&eot=812";
const URL_SUFFIX = "?device=pc&platform=web&ultra_sign={sign}&nov=27&a=810";
const UA =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36";
const CACHE_DIR = process.env.MANGA_CACHE || path.join(os.homedir(), ".bili-sync", "manga");
/** 站点构建常量，命中即可跳过探测；不命中时自动重新提取 */
const KNOWN_SN = "1E74C20E5720FBF3BB351965D7A9DFC1";

function log(...args) {
  if (process.env.MANGA_QUIET) return;
  console.error("[manga]", ...args);
}

// ---------------------------------------------------------------- HTTP

let PROXY_AGENT = null;
let PROXY_READY = false;

function proxyAgent() {
  if (PROXY_READY) return PROXY_AGENT;
  PROXY_READY = true;
  const raw = process.env.MANGA_PROXY;
  if (!raw) return (PROXY_AGENT = null);
  let proxy;
  try {
    proxy = new URL(raw);
  } catch (e) {
    log("MANGA_PROXY 解析失败，忽略：" + raw);
    return (PROXY_AGENT = null);
  }
  const agent = new https.Agent({ keepAlive: false });
  agent.createConnection = (opts, cb) => {
    const headers = {};
    if (proxy.username) {
      headers["Proxy-Authorization"] =
        "Basic " + Buffer.from(proxy.username + ":" + proxy.password).toString("base64");
    }
    const creq = http.request({
      host: proxy.hostname,
      port: proxy.port || 80,
      method: "CONNECT",
      path: opts.host + ":" + (opts.port || 443),
      headers: headers,
    });
    creq.on("connect", (res, socket) => {
      if (res.statusCode !== 200) {
        socket.destroy();
        cb(new Error("代理 CONNECT 失败 " + res.statusCode));
        return;
      }
      const tlsSocket = tls.connect({ socket: socket, servername: opts.host });
      tlsSocket.once("secureConnect", () => cb(null, tlsSocket));
      tlsSocket.once("error", (e) => cb(e));
    });
    creq.on("error", (e) => cb(e));
    creq.end();
  };
  log("使用代理 " + raw);
  return (PROXY_AGENT = agent);
}

function request(url, options, body) {
  options = options || {};
  return new Promise((resolve, reject) => {
    const opts = {
      method: options.method || "GET",
      headers: Object.assign({ "User-Agent": UA }, options.headers || {}),
    };
    const agent = proxyAgent();
    if (agent) opts.agent = agent;
    const req = https.request(url, opts, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, buf: Buffer.concat(chunks) }));
    });
    req.on("error", reject);
    if (body) req.write(body);
    req.end();
  });
}

async function fetchText(url) {
  const res = await request(url);
  if (res.status !== 200) throw new Error("拉取失败 " + res.status + "：" + url);
  return res.buf.toString("utf8");
}

async function fetchBinary(url) {
  const res = await request(url);
  if (res.status !== 200) throw new Error("下载失败 " + res.status + "：" + url);
  return res.buf;
}

function cookieHeader() {
  return process.env.MANGA_COOKIE || "";
}

function buvid3() {
  return process.env.MANGA_BUVID3 || "";
}

// ------------------------------------------------- 站点资源（wasm / 常量）

function ensureDir(dir) {
  fs.mkdirSync(dir, { recursive: true });
}

function jsAssetsFromHtml(html) {
  const names = new Set();
  const re = /manga-pc\/static\/js\/([A-Za-z0-9._-]+\.js)/g;
  let m;
  while ((m = re.exec(html)) !== null) names.add(m[1]);
  return Array.from(names);
}

function metaPath() {
  return path.join(CACHE_DIR, "meta.json");
}

function loadMeta() {
  try {
    if (fs.existsSync(metaPath())) return JSON.parse(fs.readFileSync(metaPath(), "utf8"));
  } catch (e) {
    /* 缓存损坏则忽略 */
  }
  return null;
}

function saveMeta(meta) {
  ensureDir(CACHE_DIR);
  fs.writeFileSync(metaPath(), JSON.stringify(meta, null, 2));
}

function gluePath() {
  return path.join(CACHE_DIR, "wasm_exec.js");
}

function ensureGoGlue() {
  if (!fs.existsSync(gluePath())) throw new Error("缺少 Go wasm 胶水：" + gluePath() + "，请先执行 bootstrap");
  return gluePath();
}

// ------------------------------------------------------------ 浏览器垫片

function fake(kind) {
  const target = function () {};
  target.style = {};
  target.dataset = {};
  return new Proxy(target, {
    get(t, p) {
      if (p === "then") return undefined;
      if (p in t) return t[p];
      if (p === "getContext") return () => fake("ctx");
      if (p === "toDataURL" || p === "getImageData" || p === "measureText") return () => fake("data");
      if (p === "width" || p === "height" || p === "length") return 0;
      if (p === Symbol.toPrimitive) return () => kind;
      return fake(String(p));
    },
    apply() {
      return fake("call");
    },
  });
}

function installBrowserShims() {
  if (globalThis.__mangaShims) return;
  globalThis.__mangaShims = true;
  globalThis.window = globalThis;
  const define = (name, value) => {
    try {
      Object.defineProperty(globalThis, name, { value: value, configurable: true, writable: true });
    } catch (e) {
      try {
        globalThis[name] = value;
      } catch (e2) {
        /* 只读且不可覆盖时忽略 */
      }
    }
  };
  define("navigator", {
    userAgent: UA,
    platform: "Win32",
    language: "zh-CN",
    hardwareConcurrency: 8,
    maxTouchPoints: 0,
  });
  define("location", {
    href: MANGA_ORIGIN + "/",
    origin: MANGA_ORIGIN,
    hostname: "manga.bilibili.com",
  });
  define("document", {
    cookie: cookieHeader(),
    location: globalThis.location,
    referrer: MANGA_ORIGIN + "/",
    getElementById: () => fake("el"),
    createElement: () => fake("el"),
    querySelector: () => fake("el"),
    querySelectorAll: () => [],
    getElementsByTagName: () => [],
    addEventListener() {},
    documentElement: fake("html"),
    body: fake("body"),
  });
  define("screen", { width: 1920, height: 1080, colorDepth: 24, availWidth: 1920, availHeight: 1040 });
  define(
    "XMLHttpRequest",
    class {
      open() {}
      setRequestHeader() {}
      send() {}
      addEventListener() {}
      getAllResponseHeaders() {
        return "";
      }
    }
  );
}

// ------------------------------------------------- Go 胶水 / wasm 装载

let GO_LOADED = false;

/** 执行官方 wasm_exec.js，注册 globalThis.Go（只执行一次） */
function ensureGoLoaded() {
  if (GO_LOADED) return;
  installBrowserShims();
  const glue = ensureGoGlue();
  new Function(fs.readFileSync(glue, "utf8"))();
  if (typeof globalThis.Go !== "function") throw new Error("Go 胶水未注册 globalThis.Go：" + glue);
  GO_LOADED = true;
}

/** wasmPath -> { exportName, fn }，避免重复实例化、避免依赖 globalThis 不被覆盖 */
const LOADED_WASM = new Map();

function loadWasmModule(wasmPath) {
  const cached = LOADED_WASM.get(wasmPath);
  if (cached) return cached;
  ensureGoLoaded();
  const before = new Set(Object.keys(globalThis));
  const go = new globalThis.Go();
  const inst = new WebAssembly.Instance(new WebAssembly.Module(fs.readFileSync(wasmPath)), go.importObject);
  // go.run 在首个 await 之前同步注册导出函数；返回的 promise 常驻（Go 程序不退出）
  Promise.resolve(go.run(inst)).catch((e) => log("wasm 运行时退出：" + path.basename(wasmPath) + " " + e.message));
  const names = Object.keys(globalThis).filter((k) => !before.has(k) && typeof globalThis[k] === "function");
  if (!names.length) throw new Error("wasm 未导出函数：" + wasmPath);
  const record = { exportName: names[0], exportNames: names, fn: globalThis[names[0]] };
  LOADED_WASM.set(wasmPath, record);
  return record;
}

/** 通过「探测调用」反推 wasm 角色：m2 / sign / decrypt / other */
function classifyWasm(fn) {
  try {
    const out = fn("probe");
    if (typeof out === "string" && out.length > 200) return "m2";
  } catch (e) {
    /* 继续下一项探测 */
  }
  try {
    const out = fn();
    if (out && typeof out === "object" && typeof out.error === "string") {
      if (/Expected 3 arguments/.test(out.error)) return "sign";
      if (/Expected 5 arguments/.test(out.error)) return "decrypt";
    }
  } catch (e) {
    /* 继续下一项探测 */
  }
  return "other";
}

// ---------------------------------------------------------------- 签名

let RUNTIME = null;

function runtime(meta) {
  if (RUNTIME) return RUNTIME;
  ensureGoLoaded();
  const load = (role) => {
    const info = meta.roles && meta.roles[role];
    if (!info) throw new Error("缺少 " + role + " 模块，请重新 bootstrap");
    return loadWasmModule(path.join(CACHE_DIR, info.file)).fn;
  };
  RUNTIME = { m2: load("m2"), sign: load("sign"), decrypt: load("decrypt") };
  return RUNTIME;
}

/**
 * 解密产物统一成 { error, data }：
 * 站点 wasm 的 decrypt 导出返回 { error, data }，其中 data 是内层 JSON 字符串。
 */
function decodeDecrypted(out) {
  let wrapper;
  if (typeof out === "string") wrapper = { error: "", data: out };
  else if (out instanceof Uint8Array) wrapper = { error: "", data: Buffer.from(out).toString("utf8") };
  else if (out && typeof out === "object") wrapper = out;
  else return { error: "解密返回类型异常：" + typeof out };
  if (wrapper.error) return { error: wrapper.error };
  let data = wrapper.data;
  if (typeof data === "string") {
    try {
      data = JSON.parse(data);
    } catch (e) {
      /* 非 JSON 时保留原始字符串 */
    }
  }
  return { error: "", data: data };
}

/** 发送一次 twirp 请求（body 已构造）并解密响应；不触发 bootstrap */
async function twirpSend(method, body, sn, rt, options) {
  const ts = Date.now();
  const sign = rt.sign(CANONICAL_QUERY, body, ts).sign;
  const urlPath = "/twirp/comic.v1.Comic/" + method + URL_SUFFIX.replace("{sign}", sign);
  const res = await request(
    MANGA_ORIGIN + urlPath,
    {
      method: "POST",
      headers: {
        "Content-Type": "application/json;charset=UTF-8",
        Referer: MANGA_ORIGIN + "/",
        Origin: MANGA_ORIGIN,
        "Content-Length": Buffer.byteLength(body),
        "x-bili-data-sn": sn,
        Cookie: cookieHeader(),
      },
    },
    body
  );
  let json;
  try {
    json = JSON.parse(res.buf.toString("utf8"));
  } catch (e) {
    return { error: "响应不是 JSON（HTTP " + res.status + "）" };
  }
  if (!json.bytesData) {
    // 少数接口（如 Search）的响应本来就是明文 data，不会给 bytesData；
    // 只有 code 非 0 或 data 也为空时才算被拒（例如 sn 不匹配的软拒绝）。
    if (options && options.allowPlaintext && json.code === 0 && json.data !== undefined && json.data !== null) {
      return { error: "", data: json.data, plaintext: true };
    }
    return { error: "服务端未返回密文（code=" + json.code + ", msg=" + json.msg + "）", code: json.code, msg: json.msg };
  }
  return decodeDecrypted(rt.decrypt(urlPath, json.bytesData, buvid3(), "web", body));
}

/** 按站点约定组装带 m2 的请求体后发送 */
async function sendTwirp(method, payload, seedId, sn, rt, options) {
  const ts = Date.now();
  const m2 = rt.m2(String(seedId) + "_" + (ts % 10000000));
  const body = JSON.stringify(Object.assign({}, payload, { m2: m2 }));
  return await twirpSend(method, body, sn, rt, options);
}

// ------------------------------------------------------------ bootstrap

async function downloadWasm(meta, name, force) {
  const target = path.join(CACHE_DIR, name);
  if (!force && fs.existsSync(target) && fs.statSync(target).size > 0) return target;
  log("下载 wasm " + name);
  fs.writeFileSync(target, await fetchBinary(STATIC_BASE + name));
  return target;
}

function probeRoles(wasmNames) {
  const roles = { report: [] };
  for (const name of wasmNames) {
    const target = path.join(CACHE_DIR, name);
    let rec;
    try {
      rec = loadWasmModule(target);
    } catch (e) {
      log("wasm " + name + " 加载失败：" + e.message);
      continue;
    }
    const warn = console.warn;
    console.warn = () => {};
    let role;
    try {
      role = classifyWasm(rec.fn);
    } finally {
      console.warn = warn;
    }
    if (role === "m2" || role === "sign" || role === "decrypt") {
      if (roles[role]) {
        log("角色 " + role + " 重复（" + roles[role].file + " / " + name + "），保留先到的");
      } else {
        roles[role] = { file: name, exportName: rec.exportName };
        log("角色 " + role + " = " + name + " (" + rec.exportName + ")");
      }
    } else {
      roles.report.push({ file: name, exportName: rec.exportName, exportNames: rec.exportNames });
    }
  }
  return roles;
}

function snCandidatesFrom(source) {
  const found = new Set();
  const patterns = [/'([0-9A-F]{32})'/g, /"([0-9A-F]{32})"/g, /`([0-9A-F]{32})`/g];
  for (const re of patterns) {
    let m;
    while ((m = re.exec(source)) !== null) found.add(m[1]);
  }
  return Array.from(found);
}

async function collectAssets(meta, force) {
  if (meta && meta.wasmNames && meta.wasmNames.length && !force) return meta;
  log("抓取站点资源清单…");
  const html = await fetchText(MANGA_ORIGIN + "/detail/mc25969");
  const assets = jsAssetsFromHtml(html);
  const biliJs = assets.find((n) => n.indexOf("bili.") === 0);
  if (!biliJs) throw new Error("未在页面中找到 bili.*.js");
  const biliSource = await fetchText(STATIC_BASE + "static/js/" + biliJs);
  const wasmNames = Array.from(
    new Set((biliSource.match(/"([0-9a-f]{16,}\.wasm)"/g) || []).map((s) => s.slice(1, -1)))
  );
  log("发现 " + wasmNames.length + " 个 wasm");
  meta = meta || {};
  meta.assets = assets;
  meta.biliJs = biliJs;
  meta.wasmNames = wasmNames;
  meta.siteUpdatedAt = new Date().toISOString();
  return meta;
}

async function ensureGlue(force) {
  if (force || !fs.existsSync(gluePath())) {
    log("下载 Go wasm 胶水 " + WASM_EXEC_VERSION);
    const glue = await fetchText(
      "https://raw.githubusercontent.com/golang/go/" + WASM_EXEC_VERSION + "/misc/wasm/wasm_exec.js"
    );
    ensureDir(CACHE_DIR);
    fs.writeFileSync(gluePath(), glue);
  }
}

async function collectSnCandidates(meta, force) {
  if (meta.snCandidates && meta.snCandidates.length && !force) return meta.snCandidates;
  log("提取 x-bili-data-sn 候选…");
  const readerHtml = await fetchText(MANGA_ORIGIN + "/mc25969/934687");
  const readerAssets = jsAssetsFromHtml(readerHtml);
  const readerJs = readerAssets.find((n) => n.indexOf("reader.") === 0);
  const out = [];
  if (readerJs) {
    const readerSource = await fetchText(STATIC_BASE + "static/js/" + readerJs);
    out.push.apply(out, snCandidatesFrom(readerSource));
    meta.readerJs = readerJs;
  }
  if (!out.length && meta.biliJs) {
    const biliSource = await fetchText(STATIC_BASE + "static/js/" + meta.biliJs);
    out.push.apply(out, snCandidatesFrom(biliSource));
  }
  log("候选常量 " + out.length + " 个");
  return out;
}

/** 首次运行：抓取站点资源、识别 wasm 角色、提取并自校验 x-bili-data-sn */
async function bootstrap(force) {
  ensureDir(CACHE_DIR);
  let meta = force ? null : loadMeta();
  if (meta && meta.ready && !force) return meta;

  meta = await collectAssets(meta, force);
  await ensureGlue(force);
  ensureGoLoaded();

  if (!meta.roles || !meta.roles.m2 || !meta.roles.sign || !meta.roles.decrypt || force) {
    for (const name of meta.wasmNames) await downloadWasm(meta, name, force);
    meta.roles = probeRoles(meta.wasmNames);
  }
  if (!meta.roles.m2 || !meta.roles.sign || !meta.roles.decrypt) {
    throw new Error("未能识别 wasm 角色（m2/sign/decrypt），站点可能已改版");
  }
  meta.ready = false;
  saveMeta(meta);

  const rt = runtime(meta);
  meta.snCandidates = await collectSnCandidates(meta, force);

  const tries = [];
  if (process.env.MANGA_SN) tries.push(process.env.MANGA_SN);
  if (meta.sn) tries.push(meta.sn);
  if (tries.indexOf(KNOWN_SN) < 0) tries.push(KNOWN_SN);
  for (const c of meta.snCandidates) if (tries.indexOf(c) < 0) tries.push(c);

  for (const sn of tries) {
    let res;
    try {
      res = await sendTwirp("ComicDetail", { comic_id: 25969 }, 25969, sn, rt);
    } catch (e) {
      log("候选 sn " + sn + " 请求异常：" + e.message);
      continue;
    }
    if (res && !res.error) {
      meta.sn = sn;
      meta.ready = true;
      meta.updatedAt = new Date().toISOString();
      saveMeta(meta);
      log("bootstrap 完成，x-bili-data-sn = " + sn);
      return meta;
    }
    log("候选 sn " + sn + " 被拒绝：" + (res && res.error ? res.error : "未知原因"));
  }
  saveMeta(meta);
  throw new Error("未能确定 x-bili-data-sn 常量（站点可能已改版，候选 " + tries.length + " 个）");
}

// ---------------------------------------------------------------- 业务

async function callTwirp(method, payload, seedId, options) {
  const meta = await bootstrap(false);
  const rt = runtime(meta);
  const res = await sendTwirp(method, payload, seedId, meta.sn, rt, options);
  if (res.error) throw new Error(method + " 失败：" + res.error);
  return res.data;
}

async function comicDetail(params) {
  const id = Number(params.comic_id || params.media_id);
  if (!id) throw new Error("缺少 comic_id");
  return { data: await callTwirp("ComicDetail", { comic_id: id }, id) };
}

async function imageIndex(params) {
  const id = Number(params.ep_id);
  if (!id) throw new Error("缺少 ep_id");
  return { data: await callTwirp("GetImageIndex", { ep_id: id }, id) };
}

/** ImageToken：body 为明文 {urls, m1}，m1 = 客户端 ECDH P-256 公钥 raw 点的 base64 */
async function imageToken(params) {
  const meta = await bootstrap(false);
  const rt = runtime(meta);
  const index = await imageIndex(params);
  const data = index.data || {};
  const paths = (data.images || []).map((i) => i.path);
  if (!paths.length) throw new Error("该话没有页面");

  const ecdh = crypto.createECDH("prime256v1");
  ecdh.generateKeys();
  const m1 = ecdh.getPublicKey().toString("base64");

  const body = JSON.stringify({ urls: JSON.stringify(paths), m1: m1 });
  const res = await twirpSend("ImageToken", body, meta.sn, rt);
  if (res.error) throw new Error("ImageToken 失败：" + res.error);
  const list = Array.isArray(res.data)
    ? res.data.map((item) => Object.assign({}, item, { complete_url: withCdnCode(item.complete_url) }))
    : res.data;
  return { data: list, m1: m1 };
}

/**
 * 给 CDN 直链补上 `code=DanmakuInfo`。
 *
 * 实测：`mangaup.hdslb.com`（原图 CDN）对不带该参数的请求直接返回
 * `400 {"code":250617,"msg":"图片获取失败"}`；浏览器阅读器请求这些图片时也带了这个参数。
 * 补上之后同一条直链立即返回 200 + 原始 JPEG。
 */
function withCdnCode(url) {
  if (!url || /[?&]code=/.test(url)) return url;
  return url + (url.includes("?") ? "&" : "?") + "code=DanmakuInfo";
}

/** 一话的完整页面清单（ImageIndex + ImageToken 合并），供下载器直接使用 */
async function chapterPages(params) {
  const epId = Number(params.ep_id);
  const index = await imageIndex(params);
  const images = (index.data && index.data.images) || [];
  const token = await imageToken(params);
  const tokens = token.data || [];
  const byPath = new Map();
  for (const t of tokens) {
    const key = t.path || t.url || "";
    byPath.set(key, t);
  }
  const pages = images.map((img, i) => {
    const t = byPath.get(img.path) || tokens[i] || {};
    return {
      index: i,
      path: img.path,
      url: withCdnCode(t.complete_url || t.url || ""),
      x: img.x || 0,
      y: img.y || 0,
      size: t.size || 0,
      token_path: t.path || "",
    };
  });
  return { ep_id: epId, count: pages.length, pages: pages };
}

/** 关键词搜索漫画（Comic/Search），供「添加视频源 → 漫画」按名字挑作品 */
async function searchComic(params) {
  const keyword = String(params.keyword || params.key_word || "").trim();
  if (!keyword) throw new Error("缺少搜索关键词");
  const pageNum = Math.max(1, Number(params.page || params.page_num || 1) || 1);
  const pageSize = Math.min(50, Math.max(1, Number(params.page_size || 20) || 20));
  const payload = {
    key_word: keyword,
    page_num: pageNum,
    page_size: pageSize,
    search_type: Number(params.search_type || 0) || 0,
  };
  const data = await callTwirp("Search", payload, pageNum * 7919 + pageSize, { allowPlaintext: true });
  const list = Array.isArray(data)
    ? data
    : (data && (data.list || data.comics || data.result)) || [];
  return { keyword: keyword, page: pageNum, page_size: pageSize, total: (data && data.total) || list.length, list: list };
}

async function download(params) {
  if (!params.url) throw new Error("缺少 url");
  const buf = await fetchBinary(params.url);
  if (params.out) {
    ensureDir(path.dirname(params.out));
    fs.writeFileSync(params.out, buf);
  }
  return { bytes: buf.length, format: sniffFormat(buf), out: params.out || null };
}

function sniffFormat(buf) {
  const magic = buf.slice(0, 4).toString("hex");
  if (magic.indexOf("ffd8ff") === 0) return "jpeg";
  if (buf.slice(4, 12).toString("ascii") === "ftypavif") return "avif";
  if (buf.slice(0, 4).toString("ascii") === "RIFF") return "webp";
  if (magic.indexOf("89504e47") === 0) return "png";
  return "unknown";
}

// ---------------------------------------------------------------- CLI

const COMMANDS = {
  bootstrap: (args) => bootstrap(!!args.force),
  "comic-detail": comicDetail,
  "image-index": imageIndex,
  "image-token": imageToken,
  "chapter-pages": chapterPages,
  search: searchComic,
  download: download,
};

async function runOne(cmd, args) {
  const fn = COMMANDS[cmd];
  if (!fn) throw new Error("未知子命令：" + cmd);
  return await fn(args || {});
}

async function serve() {
  const rl = readline.createInterface({ input: process.stdin, terminal: false });
  let queue = Promise.resolve();
  rl.on("line", (line) => {
    line = line.trim();
    if (!line) return;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch (e) {
      process.stdout.write(JSON.stringify({ ok: false, error: "请求不是合法 JSON" }) + "\n");
      return;
    }
    if (msg.cmd === "shutdown") {
      process.stdout.write(JSON.stringify({ id: msg.id || null, ok: true, result: "bye" }) + "\n");
      setTimeout(() => process.exit(0), 10);
      return;
    }
    queue = queue.then(async () => {
      const reply = { id: msg.id === undefined ? null : msg.id };
      try {
        reply.ok = true;
        reply.result = await runOne(msg.cmd, msg.args);
      } catch (e) {
        reply.ok = false;
        reply.error = e && e.message ? e.message : String(e);
      }
      process.stdout.write(JSON.stringify(reply) + "\n");
    });
  });
  rl.on("close", () => process.exit(0));
}

async function main() {
  const argv = process.argv.slice(2);
  if (argv[0] === "serve") return serve();
  const command = argv[0];
  const args = argv[1] ? JSON.parse(argv[1]) : {};
  if (command === "bootstrap") {
    const meta = await runOne("bootstrap", { force: argv.indexOf("--force") >= 0 });
    process.stdout.write(
      JSON.stringify({ ok: true, ready: !!meta.ready, sn: meta.sn, wasm: meta.wasmNames, roles: meta.roles }) + "\n"
    );
    return process.exit(0);
  }
  if (!COMMANDS[command]) {
    console.log(
      "用法: node manga-signer.cjs <bootstrap|comic-detail|image-index|image-token|chapter-pages|search|download|serve> '<json args>'"
    );
    process.exit(2);
  }
  // 契约：stdout 只输出一行紧凑 JSON（stderr 走日志），方便 Rust 侧按行解析。
  // 注意不能美化输出：多行 JSON 里像 `"讲谈社"` 这样的单个字符串行本身也是合法 JSON，
  // 会让「取最后一行可解析 JSON」的解析器误命中。
  const result = await runOne(command, args);
  process.stdout.write(JSON.stringify({ ok: true, result: result }) + "\n");
  process.exit(0);
}

main().catch((e) => {
  process.stdout.write(JSON.stringify({ ok: false, error: e && e.message ? e.message : String(e) }) + "\n");
  console.error("ERROR:", e && e.message ? e.message : e);
  process.exit(1);
});
