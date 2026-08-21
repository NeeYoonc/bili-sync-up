// douyin-signer.cjs — 抖音收藏夹/我的喜欢等受保护接口签名器
// 生成 a_bogus（官方 webmssdk/bdms）+ x-secsdk-web-signature（secsdk）
// 用法: node douyin-signer.cjs <完整URL(不含签名)> [--config <config_dir>]
"use strict";
const fs = require('node:fs');
const REAL_PROCESS = global.process;
const path = require('node:path');
// 支持用环境变量覆盖 SDK 目录（供服务端把 SDK 释放到独立目录后调用）。
const SDK_DIR = process.env.BILI_SYNC_DOUYIN_SDK_DIR || path.join(__dirname, 'douyin-sdk');
let cfgIdx = process.argv.indexOf('--config');
const CFG_DIR = process.env.BILI_SYNC_CONFIG_DIR || (cfgIdx > -1 ? process.argv[cfgIdx + 1] : '');
const targetUrl = process.argv[2];
const RESULT = '__SIGN_RESULT__';
function fail(msg) { console.log(RESULT + JSON.stringify({ ok: false, error: msg })); if (global.process) global.process.exit(1); else REAL_PROCESS.exit(1); }
if (!targetUrl) fail('缺少目标 URL');

// ---------- 加载 secsdk 环境（douyin-secsdk.json） ----------
const keysPath = path.join(CFG_DIR, 'douyin-secsdk.json');
let liveEnv = null;
try { liveEnv = JSON.parse(fs.readFileSync(keysPath, 'utf8')); } catch (e) { fail('缺少抖音 secsdk 密钥文件 ' + keysPath + '：请在电脑浏览器打开抖音后，用登录助手导出（含 s_sdk_crypt_sdk / s_sdk_server_cert_key），或重新导入 cookies'); }
if (!liveEnv.localStorage) fail('douyin-secsdk.json 缺少 localStorage（请重新用登录助手导出抖音登录状态）');

// cookies（覆盖导出中的 cookie，始终使用最新导入）
let cookieStr = '';
try {
  const txt = fs.readFileSync(path.join(CFG_DIR, 'douyin-cookies.txt'), 'utf8');
  cookieStr = txt.split(/\r?\n/).filter(l => l.trim() && !l.startsWith('#')).map(l => { const p = l.split('\t'); return p.length >= 7 ? p[5] + '=' + p[6] : null; }).filter(Boolean).join('; ');
} catch (e) { fail('缺少抖音 cookies.txt：请先导入抖音 cookies'); }
liveEnv.cookie = cookieStr;
liveEnv.ua = liveEnv.ua || 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0';
liveEnv.href = liveEnv.href || 'https://www.douyin.com/jingxuan';
liveEnv.ssr_user_id = liveEnv.ssr_user_id || '';

// ---------- 第一步：a_bogus（bdms webmssdk） ----------
function generateABogus(url) {
  global._process = global.process;
  try { delete global.process; } catch (e) {}
  const envUtils = require(path.join(SDK_DIR, 'bdms-env.js'));
  require(path.join(SDK_DIR, 'bdms.js'));
  if (envUtils && envUtils.restoreProcess) envUtils.restoreProcess();
  if (envUtils && envUtils.window) envUtils.window.a_bogus = null;
  const originalSet = URLSearchParams.prototype.set;
  URLSearchParams.prototype.set = function (key, value) {
    if (key === 'a_bogus' && envUtils && envUtils.window) { envUtils.window.a_bogus = value; }
    return originalSet.call(this, key, value);
  };
  try { if (globalThis.bdms && globalThis.bdms.init) globalThis.bdms.init({ aid: 6383 }); } catch (e) {}
  if (envUtils && envUtils.simulateMouseTrack) envUtils.simulateMouseTrack({ points: 20, duration: 500 });
  const xhr = new (envUtils.window.XMLHttpRequest || globalThis.XMLHttpRequest)();
  const invokeList = [
    { "args": ["GET", url, true], "func": function () {} },
    { "args": ["Accept", "application/json,text/plain,*/*"], "func": function () {} },
    { "args": ["uifid", ""] }
  ];
  xhr.bdmsInvokeList = invokeList;
  try { xhr.send(null); } catch (e) { /* VM 后续步骤可能报错，a_bogus 已在 URLSearchParams hook 中捕获 */ }
  return envUtils.window ? envUtils.window.a_bogus : null;
}

// ---------- 第二步：secsdk（x-secsdk-web-signature） ----------
function signWebUrl(urlWithABogus) {
  globalThis.__DY_SECSDK_ENV__ = liveEnv;
  const envPath = path.join(SDK_DIR, 'websign-env.js');
  delete require.cache[require.resolve(envPath)];
  const env = require(envPath);
  const win = env.window;
  const code = fs.readFileSync(path.join(SDK_DIR, 'secsdk-runtime.js'), 'utf8');
  const runner = new Function('window','document','navigator','location','localStorage','sessionStorage','screen','performance','self','globalThis', code);
  runner.call(win, win, env.document, win.navigator, win.location, win.localStorage, win.sessionStorage, win.screen, win.performance, win, win);
  if (env.restoreProcess) env.restoreProcess();
  const f = win.use && win.use('webSignUrl');
  if (typeof f !== 'function') throw new Error('webSignUrl 未注册');
  const r = f(urlWithABogus);
  if (typeof r === 'string') return r;
  if (r && typeof r.url === 'string') return r.url;
  throw new Error('webSignUrl 返回为空（secsdk 会话密钥可能过期，请重新用登录助手导出抖音登录状态）');
}

try {
  const aBogus = generateABogus(targetUrl);
  if (!aBogus) fail('a_bogus 生成失败：官方 webmssdk 未捕获到签名（SDK 可能已更新，请在浏览器打开抖音后重新抓取最新 webmssdk/bdms）');
  const sep = targetUrl.includes('?') ? '&' : '?';
  const urlWithABogus = targetUrl + sep + 'a_bogus=' + aBogus;
  const signed = signWebUrl(urlWithABogus);
  if (!signed || !signed.includes('x-secsdk-web-signature')) fail('secsdk 签名生成失败（返回：' + String(signed).slice(0, 120) + '）');
  console.log(RESULT + JSON.stringify({ ok: true, signed_url: signed }));
  if (global.process) global.process.exit(0); else REAL_PROCESS.exit(0);
} catch (e) {
  fail(String((e && e.stack) || e));
}
