/**
 * x-secsdk-web-signature 补环境（Node）
 *
 * 思路（方案 1）：在 Node 里补出浏览器环境，预置实时浏览器导出的 secsdk 会话密钥
 * （security-sdk/s_sdk_crypt_sdk 等，存进 mock 的 localStorage），然后按真实页面顺序
 * 加载 secsdk_runtime.js + bdms，触发 webSignUrl 注册，最后调用
 * window.use('webSignUrl')(url) 直接拿到带 uifid+timestamp+x-secsdk-web-signature 的 URL。
 *
 * 免握手：secsdk 初始化时从 localStorage 恢复密钥，不再向服务器握手即可同步签名。
 */
const path = require('path');
const fs = require('fs');

const LIB = __dirname;
// 优先使用签名器注入的实时环境（douyin-secsdk.json），否则回退到旧路径 websign_env.json
let liveEnv = null;
if (globalThis.__DY_SECSDK_ENV__) {
    liveEnv = globalThis.__DY_SECSDK_ENV__;
} else {
    const ENV_JSON = process.env.BILI_SYNC_SECSDK_ENV_JSON || path.join(LIB, '..', '..', 'reverse', 'websign_env.json');
    liveEnv = JSON.parse(fs.readFileSync(ENV_JSON, 'utf8'));
}

// ---- 2. 隐藏 Node 特征（secsdk/bdms 会检测 process）----
const _process = global.process;
global._process = _process;  // 暴露给外部恢复
try { delete global.process; } catch (e) {}

// ---- 3. 构造浏览器全局对象 ----
// localStorage：预置 live 导出的密钥
function makeStorage(initial) {
    const store = Object.assign({}, initial || {});
    return {
        getItem: function (k) { return Object.prototype.hasOwnProperty.call(store, k) ? store[k] : null; },
        setItem: function (k, v) { store[k] = String(v); },
        removeItem: function (k) { delete store[k]; },
        clear: function () { for (const k in store) delete store[k]; },
        key: function (i) { return Object.keys(store)[i] || null; },
        get length() { return Object.keys(store).length; },
        _store: store,
    };
}

const localStorage = makeStorage(liveEnv.localStorage);
const sessionStorage = makeStorage({});

// cookie：用 live 导出的 cookie 字符串
let _cookie = liveEnv.cookie || '';

const navigator = {
    userAgent: liveEnv.ua || 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36',
    platform: 'Win32',
    language: 'zh-CN',
    languages: ['zh-CN', 'zh'],
    appVersion: '5.0 (Windows NT 10.0; Win64; x64)',
    appName: 'Netscape',
    product: 'Gecko',
    productSub: '20030107',
    vendor: 'Google Inc.',
    hardwareConcurrency: 12,
    deviceMemory: 16,
    webdriver: false,
    onLine: true,
    cookieEnabled: true,
    sendBeacon: function () { return true; },
    maxTouchPoints: 0,
};

// location：用 live 的 href
const _hrefURL = new URL(liveEnv.href || 'https://www.douyin.com/jingxuan');
const location = {
    href: _hrefURL.href,
    protocol: _hrefURL.protocol,
    host: _hrefURL.host,
    hostname: _hrefURL.hostname,
    port: _hrefURL.port,
    pathname: _hrefURL.pathname,
    search: _hrefURL.search,
    hash: _hrefURL.hash,
    origin: _hrefURL.origin,
    replace: function () {},
    assign: function () {},
    reload: function () {},
    toString: function () { return _hrefURL.href; },
};

// 极简 DOM 节点
function makeEl(tag) {
    return {
        tagName: (tag || 'div').toUpperCase(),
        nodeName: (tag || 'div').toUpperCase(),
        style: {}, dataset: {}, attributes: {}, childNodes: [], children: [],
        setAttribute: function (k, v) { this.attributes[k] = v; },
        getAttribute: function (k) { return this.attributes[k]; },
        removeAttribute: function (k) { delete this.attributes[k]; },
        appendChild: function (c) { this.childNodes.push(c); return c; },
        removeChild: function (c) { return c; },
        insertBefore: function (c) { this.childNodes.push(c); return c; },
        addEventListener: function () {}, removeEventListener: function () {},
        dispatchEvent: function () { return true; },
        contains: function () { return false; },
        querySelector: function () { return null; },
        querySelectorAll: function () { return []; },
        getElementsByTagName: function () { return [headEl]; },
        cloneNode: function () { return makeEl(this.tagName); },
        getContext: function () { return null; },
    };
}
const headEl = makeEl('head');
const bodyEl = makeEl('body');
const htmlEl = makeEl('html');

const document = {
    nodeType: 9,
    readyState: 'complete',
    currentScript: { getAttribute: function () { return null; } },
    documentElement: htmlEl,
    head: headEl,
    body: bodyEl,
    cookie: '',  // 用 getter/setter 替换，见下
    referrer: 'https://www.douyin.com/',
    title: '抖音',
    location: location,
    URL: location.href,
    domain: location.hostname,
    characterSet: 'UTF-8',
    createElement: function (tag) { return makeEl(tag); },
    createElementNS: function (_ns, tag) { return makeEl(tag); },
    createTextNode: function (t) { return { nodeType: 3, textContent: t }; },
    getElementById: function () { return null; },
    getElementsByTagName: function (t) {
        if (t === 'head') return [headEl];
        if (t === 'body') return [bodyEl];
        return [];
    },
    getElementsByClassName: function () { return []; },
    querySelector: function () { return null; },
    querySelectorAll: function () { return []; },
    addEventListener: function () {}, removeEventListener: function () {},
    dispatchEvent: function () { return true; },
    createEvent: function () { return { initEvent: function () {} }; },
};
Object.defineProperty(document, 'cookie', {
    get: function () { return _cookie; },
    set: function (v) {
        // 朴素 cookie 合并：取 name=value 部分
        const pair = String(v).split(';')[0];
        const name = pair.split('=')[0];
        const parts = _cookie ? _cookie.split('; ').filter(Boolean) : [];
        const kept = parts.filter(function (p) { return p.split('=')[0] !== name; });
        kept.push(pair);
        _cookie = kept.join('; ');
    },
    configurable: true,
});

const screen = { width: 1707, height: 1067, availWidth: 1707, availHeight: 1067, colorDepth: 24, pixelDepth: 24 };

const performanceObj = {
    now: function () { return 12345.678; },
    timing: {}, navigation: {}, getEntriesByType: function () { return []; },
};

// crypto：用 Node 18+ 的 WebCrypto（ECDSA P-256 由 secsdk 内部纯 JS 实现，这里作兜底）
const nodeCrypto = require('crypto');
const cryptoObj = global.crypto || (nodeCrypto.webcrypto) || {
    getRandomValues: function (arr) {
        const b = nodeCrypto.randomBytes(arr.length);
        for (let i = 0; i < arr.length; i++) arr[i] = b[i];
        return arr;
    },
};
if (!cryptoObj.getRandomValues) {
    cryptoObj.getRandomValues = function (arr) {
        const b = nodeCrypto.randomBytes(arr.length);
        for (let i = 0; i < arr.length; i++) arr[i] = b[i];
        return arr;
    };
}

// ---- 4. 组装 window ----
const window = global.window || global;
const fakes = {
    window: window, self: window, top: window, parent: window, frames: window,
    document: document, navigator: navigator, location: location, screen: screen,
    localStorage: localStorage, sessionStorage: sessionStorage,
    performance: performanceObj, crypto: cryptoObj,
    history: { length: 1, pushState: function () {}, replaceState: function () {}, back: function () {}, go: function () {} },
    addEventListener: function () {}, removeEventListener: function () {}, dispatchEvent: function () { return true; },
    setTimeout: global.setTimeout, clearTimeout: global.clearTimeout,
    setInterval: global.setInterval, clearInterval: global.clearInterval,
    requestAnimationFrame: function (cb) { return global.setTimeout(function () { cb(Date.now()); }, 16); },
    cancelAnimationFrame: function (id) { return global.clearTimeout(id); },
    atob: function (s) { return Buffer.from(s, 'base64').toString('binary'); },
    btoa: function (s) { return Buffer.from(s, 'binary').toString('base64'); },
    fetch: function () { return Promise.resolve({ ok: true, status: 200, json: function () { return Promise.resolve({}); }, text: function () { return Promise.resolve(''); } }); },
    matchMedia: function () { return { matches: false, addListener: function () {}, removeListener: function () {} }; },
    getComputedStyle: function () { return {}; },
    Image: function () { return makeEl('img'); },
    XMLHttpRequest: function () {
        return {
            open: function () {}, send: function () {}, setRequestHeader: function () {},
            addEventListener: function () {}, getResponseHeader: function () { return null; },
            readyState: 4, status: 200, responseText: '', response: '',
        };
    },
};
for (const k in fakes) {
    if (typeof window[k] === 'undefined') {
        try { window[k] = fakes[k]; } catch (e) {}
    }
}
// 强制覆盖关键对象（即便 global 已有）
window.document = document;
window.navigator = navigator;
window.location = location;
window.localStorage = localStorage;
window.sessionStorage = sessionStorage;
window.screen = screen;
if (!window.crypto || !window.crypto.subtle) window.crypto = cryptoObj;

// 注入 SSR 用户态（webSign 会读 user_id 写 _secsdk_uifid）
window.SSR_RENDER_DATA = liveEnv.ssr_user_id
    ? { app: { odin: { user_id: liveEnv.ssr_user_id } } }
    : { app: { odin: {} } };

module.exports = {
    window: window,
    document: document,
    navigator: navigator,
    localStorage: localStorage,
    restoreProcess: function () { if (_process) global.process = _process; },
    liveEnv: liveEnv,
};
