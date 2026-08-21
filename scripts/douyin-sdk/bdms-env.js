/**
 * bdms.js 运行环境模拟
 * 用于在 Node.js 中运行 bdms.js
 */

// 隐藏 Node.js 环境特征，让 bdms.js 认为在浏览器中运行
// bdms.js 564行检测: "undefined" != typeof process && "process" == n(process)
global._process = global.process;
delete global.process;

// 最先定义全局函数，避免 bdms 加载时找不到
global.requestAnimationFrame = function(cb) { return setTimeout(cb, 16); };
global.cancelAnimationFrame = function(id) { clearTimeout(id); };

// 破坏 URLSearchParams 检测，让 bdms 重写 fetch
// 模块 5406 检测：!r.size && (u || !i)
// 让 size 属性不存在，检测就会失败
Object.defineProperty(URLSearchParams.prototype, 'size', {
    get: function() {
        throw new Error('size not supported');
    },
    configurable: true
});

// 全局变量存储 a_bogus
var windows = {
    a_bogus: null
};

// 鼠标轨迹数据存储（bdms 会读取这些数据用于签名验证）
var _mouseTrackData = {
    // 鼠标移动轨迹点
    movements: [],
    // 最后一次鼠标位置
    lastX: 0,
    lastY: 0,
    // 开始时间
    startTime: Date.now(),
    // 总移动距离
    totalDistance: 0,
    // 移动次数
    moveCount: 0
};

// 模拟 window 对象
var window = {
    bdms: null,
    __ac_referer: "",
    // bdms 数据存储对象
    _bd_data: _mouseTrackData,
    __bdms_data: _mouseTrackData,
    location: {
        href: "https://www.douyin.com/",
        pathname: "/",
        search: "",
        host: "www.douyin.com",
        hostname: "www.douyin.com",
        protocol: "https:",
        origin: "https://www.douyin.com"
    },
    navigator: {
        userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
        platform: "Win32",
        language: "zh-CN",
        languages: ["zh-CN", "zh"],
        cookieEnabled: true,
        onLine: true,
        hardwareConcurrency: 16,
        deviceMemory: 8,
        maxTouchPoints: 0,
        appName: "Netscape",
        appVersion: "5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        vendor: "Google Inc.",
        vendorSub: "",
        productSub: "20030107",
        product: "Gecko",
        appCodeName: "Mozilla",
        doNotTrack: null,
        // 模拟真实浏览器插件
        plugins: (function() {
            var plugins = [
                { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
                { name: 'Chrome PDF Viewer', filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai', description: '' },
                { name: 'Native Client', filename: 'internal-nacl-plugin', description: '' }
            ];
            plugins.length = 3;
            plugins.item = function(i) { return this[i] || null; };
            plugins.namedItem = function(name) {
                for (var i = 0; i < this.length; i++) {
                    if (this[i].name === name) return this[i];
                }
                return null;
            };
            plugins.refresh = function() {};
            return plugins;
        })(),
        mimeTypes: (function() {
            var mimeTypes = [
                { type: 'application/pdf', suffixes: 'pdf', description: 'Portable Document Format' },
                { type: 'text/pdf', suffixes: 'pdf', description: 'Portable Document Format' }
            ];
            mimeTypes.length = 2;
            mimeTypes.item = function(i) { return this[i] || null; };
            mimeTypes.namedItem = function(name) {
                for (var i = 0; i < this.length; i++) {
                    if (this[i].type === name) return this[i];
                }
                return null;
            };
            return mimeTypes;
        })(),
        webdriver: false,
        getBattery: function() {
            return Promise.resolve({
                charging: true,
                chargingTime: 0,
                dischargingTime: Infinity,
                level: 1
            });
        },
        sendBeacon: function() { return true; },
        getGamepads: function() { return []; },
        javaEnabled: function() { return false; },
        vibrate: function() { return false; },
        requestMediaKeySystemAccess: function() { return Promise.reject(); },
        mediaDevices: {
            enumerateDevices: function() { return Promise.resolve([]); },
            getUserMedia: function() { return Promise.reject(); }
        },
        permissions: {
            query: function() { return Promise.resolve({ state: 'prompt' }); }
        },
        connection: {
            effectiveType: '4g',
            downlink: 10,
            rtt: 50,
            saveData: false
        },
        storage: {
            estimate: function() { return Promise.resolve({ quota: 0, usage: 0 }); }
        },
        clipboard: {
            writeText: function() { return Promise.resolve(); },
            readText: function() { return Promise.resolve(''); }
        },
        credentials: {
            get: function() { return Promise.resolve(null); },
            create: function() { return Promise.resolve(null); }
        },
        serviceWorker: null,
        geolocation: null
    },
    screen: {
        width: 1920,
        height: 1080,
        availWidth: 1920,
        availHeight: 1040,
        colorDepth: 24,
        pixelDepth: 24
    },
    devicePixelRatio: 1,
    innerWidth: 1920,
    innerHeight: 900,
    outerWidth: 1920,
    outerHeight: 1040,
    screenX: 0,
    screenY: 0,
    pageXOffset: 0,
    pageYOffset: 0,
    scrollX: 0,
    scrollY: 0,
    localStorage: {
        getItem: function(key) { return null; },
        setItem: function(key, value) {},
        removeItem: function(key) {},
        clear: function() {}
    },
    sessionStorage: {
        getItem: function(key) { return null; },
        setItem: function(key, value) {},
        removeItem: function(key) {},
        clear: function() {}
    },
    origin: "https://www.douyin.com",
    isSecureContext: true,
    indexedDB: {},
    caches: {},
    cookieStore: {},
    performance: {
        now: function() { return Date.now(); },
        timing: {
            navigationStart: Date.now() - 1000
        }
    },
    history: {
        length: 1
    },
    crypto: {
        getRandomValues: function(arr) {
            for (var i = 0; i < arr.length; i++) {
                arr[i] = Math.floor(Math.random() * 256);
            }
            return arr;
        }
    },
    _eventListeners: {},
    addEventListener: function(type, listener, options) {
        if (!this._eventListeners[type]) {
            this._eventListeners[type] = [];
        }
        this._eventListeners[type].push(listener);
    },
    removeEventListener: function(type, listener) {
        if (this._eventListeners[type]) {
            var idx = this._eventListeners[type].indexOf(listener);
            if (idx > -1) {
                this._eventListeners[type].splice(idx, 1);
            }
        }
    },
    dispatchEvent: function(event) {
        var type = event.type;
        if (this._eventListeners[type]) {
            for (var i = 0; i < this._eventListeners[type].length; i++) {
                try {
                    this._eventListeners[type][i].call(this, event);
                } catch(e) {}
            }
        }
        return true;
    },
    setTimeout: setTimeout,
    setInterval: setInterval,
    clearTimeout: clearTimeout,
    clearInterval: clearInterval,
    requestAnimationFrame: function(cb) { return setTimeout(cb, 16); },
    cancelAnimationFrame: function(id) { clearTimeout(id); },
    atob: function(str) {
        return Buffer.from(str, 'base64').toString('binary');
    },
    btoa: function(str) {
        return Buffer.from(str, 'binary').toString('base64');
    },
    fetch: function(url, options) {
        // 模拟 fetch Response
        return Promise.resolve({
            ok: true,
            status: 200,
            statusText: 'OK',
            url: typeof url === 'string' ? url : url.url,
            headers: new Map(),
            json: function() { return Promise.resolve({}); },
            text: function() { return Promise.resolve(''); },
            blob: function() { return Promise.resolve(new Blob()); },
            arrayBuffer: function() { return Promise.resolve(new ArrayBuffer(0)); },
            clone: function() { return this; }
        });
    },
    // Chrome 浏览器特有对象（重要！用于检测是否为真实 Chrome）
    chrome: {
        app: {
            isInstalled: false,
            InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
            RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' },
            getDetails: function() { return null; },
            getIsInstalled: function() { return false; },
            installState: function(callback) { callback && callback('not_installed'); }
        },
        runtime: {
            OnInstalledReason: { CHROME_UPDATE: 'chrome_update', INSTALL: 'install', SHARED_MODULE_UPDATE: 'shared_module_update', UPDATE: 'update' },
            OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update', PERIODIC: 'periodic' },
            PlatformArch: { ARM: 'arm', ARM64: 'arm64', MIPS: 'mips', MIPS64: 'mips64', X86_32: 'x86-32', X86_64: 'x86-64' },
            PlatformNaclArch: { ARM: 'arm', MIPS: 'mips', MIPS64: 'mips64', X86_32: 'x86-32', X86_64: 'x86-64' },
            PlatformOs: { ANDROID: 'android', CROS: 'cros', LINUX: 'linux', MAC: 'mac', OPENBSD: 'openbsd', WIN: 'win' },
            RequestUpdateCheckStatus: { NO_UPDATE: 'no_update', THROTTLED: 'throttled', UPDATE_AVAILABLE: 'update_available' },
            connect: function() { return { onDisconnect: { addListener: function() {} }, onMessage: { addListener: function() {} }, postMessage: function() {} }; },
            sendMessage: function() {}
        },
        csi: function() { return { pageT: Date.now(), startE: Date.now() - 1000, onloadT: Date.now() - 500 }; },
        loadTimes: function() {
            var now = Date.now() / 1000;
            return {
                commitLoadTime: now - 1,
                connectionInfo: 'h2',
                finishDocumentLoadTime: now - 0.5,
                finishLoadTime: now - 0.3,
                firstPaintAfterLoadTime: 0,
                firstPaintTime: now - 0.8,
                navigationType: 'Navigate',
                npnNegotiatedProtocol: 'h2',
                requestTime: now - 1.2,
                startLoadTime: now - 1.1,
                wasAlternateProtocolAvailable: false,
                wasFetchedViaSpdy: true,
                wasNpnNegotiated: true
            };
        }
    }
};

// 模拟 AudioContext（重要！用于音频指纹检测）
function AudioContext() {
    this.state = 'running';
    this.sampleRate = 44100;
    this.baseLatency = 0.005333333333333333;
    this.outputLatency = 0;
    this.currentTime = 0;
    this.destination = {
        maxChannelCount: 2,
        numberOfInputs: 1,
        numberOfOutputs: 0,
        channelCount: 2,
        channelCountMode: 'explicit',
        channelInterpretation: 'speakers'
    };
    this.listener = {
        positionX: { value: 0 },
        positionY: { value: 0 },
        positionZ: { value: 0 },
        forwardX: { value: 0 },
        forwardY: { value: 0 },
        forwardZ: { value: -1 },
        upX: { value: 0 },
        upY: { value: 1 },
        upZ: { value: 0 }
    };
}
AudioContext.prototype.createOscillator = function() {
    return {
        type: 'sine',
        frequency: { value: 440 },
        detune: { value: 0 },
        connect: function() { return this; },
        start: function() {},
        stop: function() {},
        disconnect: function() {}
    };
};
AudioContext.prototype.createDynamicsCompressor = function() {
    return {
        threshold: { value: -24 },
        knee: { value: 30 },
        ratio: { value: 12 },
        attack: { value: 0.003 },
        release: { value: 0.25 },
        reduction: { value: 0 },
        connect: function() { return this; },
        disconnect: function() {}
    };
};
AudioContext.prototype.createAnalyser = function() {
    return {
        fftSize: 2048,
        frequencyBinCount: 1024,
        minDecibels: -100,
        maxDecibels: -30,
        smoothingTimeConstant: 0.8,
        connect: function() { return this; },
        disconnect: function() {},
        getFloatFrequencyData: function(arr) { for (var i = 0; i < arr.length; i++) arr[i] = -100 + Math.random() * 70; },
        getByteFrequencyData: function(arr) { for (var i = 0; i < arr.length; i++) arr[i] = Math.floor(Math.random() * 256); }
    };
};
AudioContext.prototype.createGain = function() {
    return {
        gain: { value: 1 },
        connect: function() { return this; },
        disconnect: function() {}
    };
};
AudioContext.prototype.createScriptProcessor = function(bufferSize, numInputChannels, numOutputChannels) {
    return {
        bufferSize: bufferSize || 4096,
        onaudioprocess: null,
        connect: function() { return this; },
        disconnect: function() {}
    };
};
AudioContext.prototype.close = function() { this.state = 'closed'; return Promise.resolve(); };
AudioContext.prototype.resume = function() { this.state = 'running'; return Promise.resolve(); };
AudioContext.prototype.suspend = function() { this.state = 'suspended'; return Promise.resolve(); };

// OfflineAudioContext
function OfflineAudioContext(numChannels, length, sampleRate) {
    AudioContext.call(this);
    this.length = length;
}
OfflineAudioContext.prototype = Object.create(AudioContext.prototype);
OfflineAudioContext.prototype.startRendering = function() {
    var self = this;
    return new Promise(function(resolve) {
        // 返回模拟的 AudioBuffer
        resolve({
            length: self.length || 44100,
            duration: (self.length || 44100) / 44100,
            sampleRate: 44100,
            numberOfChannels: 2,
            getChannelData: function(channel) {
                // 返回伪随机但一致的音频数据（用于指纹）
                var data = new Float32Array(self.length || 44100);
                var seed = 12345;
                for (var i = 0; i < data.length; i++) {
                    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
                    data[i] = (seed / 0x7fffffff) * 2 - 1;
                }
                return data;
            }
        });
    });
};

// 模拟 XMLHttpRequest 类
function XMLHttpRequest() {
    this.readyState = 0;
    this.status = 0;
    this.statusText = '';
    this.responseText = '';
    this.responseType = '';
    this.response = null;
    this.timeout = 0;
    this.withCredentials = false;
    this._url = '';
    this._method = 'GET';
    this._headers = {};
    this._listeners = {};
}

XMLHttpRequest.prototype.open = function(method, url, async) {
    this._method = method;
    this._url = url;
    this.readyState = 1;
};

XMLHttpRequest.prototype.setRequestHeader = function(name, value) {
    this._headers[name] = value;
};

XMLHttpRequest.prototype.send = function(data) {
    var self = this;
    this.readyState = 4;
    this.status = 200;
    this.statusText = 'OK';
    this.responseText = '{}';
    this.response = {};

    // 触发 readystatechange 事件
    if (typeof this.onreadystatechange === 'function') {
        this.onreadystatechange();
    }

    // 触发 load 事件
    if (typeof this.onload === 'function') {
        this.onload();
    }

    // 触发事件监听器
    this._triggerEvent('readystatechange');
    this._triggerEvent('load');
};

XMLHttpRequest.prototype.abort = function() {
    this.readyState = 0;
};

XMLHttpRequest.prototype.addEventListener = function(type, listener) {
    if (!this._listeners[type]) {
        this._listeners[type] = [];
    }
    this._listeners[type].push(listener);
};

XMLHttpRequest.prototype.removeEventListener = function(type, listener) {
    if (this._listeners[type]) {
        var idx = this._listeners[type].indexOf(listener);
        if (idx > -1) {
            this._listeners[type].splice(idx, 1);
        }
    }
};

XMLHttpRequest.prototype._triggerEvent = function(type) {
    if (!this._listeners) {
        this._listeners = {};
    }
    var listeners = this._listeners[type];
    if (listeners) {
        var event = { type: type, target: this };
        for (var i = 0; i < listeners.length; i++) {
            listeners[i].call(this, event);
        }
    }
};

XMLHttpRequest.prototype.getResponseHeader = function(name) {
    return null;
};

XMLHttpRequest.prototype.getAllResponseHeaders = function() {
    return '';
};

// window 上挂载 XMLHttpRequest
window.XMLHttpRequest = XMLHttpRequest;

// 模拟 document 对象
var document = {
    domain: "www.douyin.com",
    referrer: "",
    cookie: "",
    title: "抖音",
    URL: "https://www.douyin.com/",
    visibilityState: "visible",
    hidden: false,
    hasFocus: function() { return true; },
    activeElement: null,
    documentElement: {
        clientWidth: 1920,
        clientHeight: 900
    },
    head: {},
    body: {
        clientWidth: 1920,
        clientHeight: 900
    },
    all: [],
    createElement: function(tag) {
        tag = tag.toLowerCase();
        if (tag === 'canvas') {
            return new HTMLCanvasElement();
        }
        return {
            tagName: tag.toUpperCase(),
            style: {},
            setAttribute: function() {},
            getAttribute: function() { return null; },
            appendChild: function() {},
            removeChild: function() {},
            addEventListener: function() {},
            removeEventListener: function() {},
            getContext: function() { return null; }
        };
    },
    getElementById: function() { return null; },
    getElementsByTagName: function() { return []; },
    getElementsByClassName: function() { return []; },
    querySelector: function() { return null; },
    querySelectorAll: function() { return []; },
    _eventListeners: {},
    addEventListener: function(type, listener, options) {
        if (!this._eventListeners[type]) {
            this._eventListeners[type] = [];
        }
        this._eventListeners[type].push(listener);
    },
    removeEventListener: function(type, listener) {
        if (this._eventListeners[type]) {
            var idx = this._eventListeners[type].indexOf(listener);
            if (idx > -1) {
                this._eventListeners[type].splice(idx, 1);
            }
        }
    },
    dispatchEvent: function(event) {
        var type = event.type;
        if (this._eventListeners[type]) {
            for (var i = 0; i < this._eventListeners[type].length; i++) {
                try {
                    this._eventListeners[type][i].call(this, event);
                } catch(e) {}
            }
        }
        return true;
    },
    createEvent: function() {
        return {
            initEvent: function() {}
        };
    }
};

// 模拟 navigator
var navigator = window.navigator;

// 模拟 location
var location = window.location;

// 模拟 screen
var screen = window.screen;

// 模拟 XMLHttpRequest
function XMLHttpRequest() {
    this.readyState = 0;
    this.status = 0;
    this.statusText = '';
    this.responseText = '';
    this.response = '';
    this.responseType = '';
    this.timeout = 0;
    this.withCredentials = false;
    this._headers = {};
}
XMLHttpRequest.prototype.open = function(method, url, async) {
    this._method = method;
    this._url = url;
    this._async = async !== false;
    this.readyState = 1;
};
XMLHttpRequest.prototype.setRequestHeader = function(name, value) {
    this._headers[name] = value;
};
XMLHttpRequest.prototype.send = function(data) {
    this.readyState = 4;
    this.status = 200;
    // 不真正发送请求，只是模拟
};
XMLHttpRequest.prototype.abort = function() {};
XMLHttpRequest.prototype.getAllResponseHeaders = function() { return ''; };
XMLHttpRequest.prototype.getResponseHeader = function(name) { return null; };
XMLHttpRequest.prototype.addEventListener = function() {};
XMLHttpRequest.prototype.removeEventListener = function() {};
XMLHttpRequest.UNSENT = 0;
XMLHttpRequest.OPENED = 1;
XMLHttpRequest.HEADERS_RECEIVED = 2;
XMLHttpRequest.LOADING = 3;
XMLHttpRequest.DONE = 4;

// 模拟 Image
function Image(width, height) {
    this.width = width || 0;
    this.height = height || 0;
    this.src = '';
    this.onload = null;
    this.onerror = null;
    this.complete = false;
    this.naturalWidth = 0;
    this.naturalHeight = 0;
}

// 模拟 Audio
function Audio(src) {
    this.src = src || '';
    this.volume = 1;
    this.muted = false;
    this.paused = true;
    this.play = function() { return Promise.resolve(); };
    this.pause = function() {};
    this.load = function() {};
}

// 模拟 MutationObserver
function MutationObserver(callback) {
    this._callback = callback;
}
MutationObserver.prototype.observe = function(target, options) {};
MutationObserver.prototype.disconnect = function() {};
MutationObserver.prototype.takeRecords = function() { return []; };

// 模拟 ResizeObserver
function ResizeObserver(callback) {
    this._callback = callback;
}
ResizeObserver.prototype.observe = function(target) {};
ResizeObserver.prototype.unobserve = function(target) {};
ResizeObserver.prototype.disconnect = function() {};

// 模拟 IntersectionObserver
function IntersectionObserver(callback, options) {
    this._callback = callback;
    this._options = options;
}
IntersectionObserver.prototype.observe = function(target) {};
IntersectionObserver.prototype.unobserve = function(target) {};
IntersectionObserver.prototype.disconnect = function() {};

// 模拟 PerformanceObserver
function PerformanceObserver(callback) {
    this._callback = callback;
}
PerformanceObserver.prototype.observe = function(options) {};
PerformanceObserver.prototype.disconnect = function() {};

// 模拟 WebSocket
function WebSocket(url, protocols) {
    this.url = url;
    this.readyState = 0;
    this.onopen = null;
    this.onclose = null;
    this.onerror = null;
    this.onmessage = null;
}
WebSocket.prototype.send = function(data) {};
WebSocket.prototype.close = function() { this.readyState = 3; };
WebSocket.CONNECTING = 0;
WebSocket.OPEN = 1;
WebSocket.CLOSING = 2;
WebSocket.CLOSED = 3;

// 模拟 Worker
function Worker(url) {
    this.onmessage = null;
    this.onerror = null;
}
Worker.prototype.postMessage = function(data) {};
Worker.prototype.terminate = function() {};

// 模拟 Blob
function Blob(parts, options) {
    this.size = 0;
    this.type = options && options.type || '';
    if (parts) {
        for (var i = 0; i < parts.length; i++) {
            if (typeof parts[i] === 'string') {
                this.size += parts[i].length;
            } else if (parts[i] instanceof ArrayBuffer) {
                this.size += parts[i].byteLength;
            }
        }
    }
}
Blob.prototype.slice = function(start, end, type) { return new Blob(); };
Blob.prototype.text = function() { return Promise.resolve(''); };
Blob.prototype.arrayBuffer = function() { return Promise.resolve(new ArrayBuffer(0)); };

// 模拟 File
function File(parts, name, options) {
    Blob.call(this, parts, options);
    this.name = name;
    this.lastModified = Date.now();
}
File.prototype = Object.create(Blob.prototype);

// 模拟 FileReader
function FileReader() {
    this.result = null;
    this.readyState = 0;
    this.onload = null;
    this.onerror = null;
}
FileReader.prototype.readAsText = function(blob) {};
FileReader.prototype.readAsDataURL = function(blob) {};
FileReader.prototype.readAsArrayBuffer = function(blob) {};

// 模拟 Canvas 相关对象
function CanvasRenderingContext2D() {
    this.canvas = null;
    this.fillStyle = '#000000';
    this.strokeStyle = '#000000';
    this.lineWidth = 1;
    this.font = '10px sans-serif';
    this.textAlign = 'start';
    this.textBaseline = 'alphabetic';
    this.globalAlpha = 1;
    this.globalCompositeOperation = 'source-over';
    // 记录绘制历史用于生成指纹
    this._drawHistory = [];
}
CanvasRenderingContext2D.prototype._record = function(op) {
    this._drawHistory.push(op);
    // 限制历史长度
    if (this._drawHistory.length > 100) {
        this._drawHistory = this._drawHistory.slice(-50);
    }
};
CanvasRenderingContext2D.prototype.fillRect = function(x, y, w, h) {
    this._record('fillRect:' + x + ',' + y + ',' + w + ',' + h + ',' + this.fillStyle);
};
CanvasRenderingContext2D.prototype.strokeRect = function(x, y, w, h) {
    this._record('strokeRect:' + x + ',' + y + ',' + w + ',' + h);
};
CanvasRenderingContext2D.prototype.clearRect = function(x, y, w, h) {
    this._record('clearRect:' + x + ',' + y + ',' + w + ',' + h);
};
CanvasRenderingContext2D.prototype.fillText = function(text, x, y) {
    this._record('fillText:' + text + ',' + x + ',' + y + ',' + this.font);
};
CanvasRenderingContext2D.prototype.strokeText = function(text, x, y) {
    this._record('strokeText:' + text + ',' + x + ',' + y);
};
CanvasRenderingContext2D.prototype.measureText = function(text) {
    // 模拟真实的文字宽度测量（基于字体）
    // 字体检测通过比较不同字体渲染相同文本的宽度差异来判断字体是否安装
    var fontSize = parseInt(this.font) || 10;
    var fontFamily = (this.font.match(/["']?([^"',]+)["']?(?:,|$)/i) || ['', 'sans-serif'])[1].trim().toLowerCase();

    // 不同字体的字符宽度系数（模拟真实浏览器）
    var fontWidthFactors = {
        'arial': 0.52,
        'helvetica': 0.52,
        'times new roman': 0.45,
        'times': 0.45,
        'georgia': 0.48,
        'verdana': 0.58,
        'courier new': 0.60,
        'courier': 0.60,
        'comic sans ms': 0.54,
        'impact': 0.42,
        'trebuchet ms': 0.50,
        'arial black': 0.62,
        'lucida console': 0.60,
        'tahoma': 0.52,
        'microsoft yahei': 0.95,
        '微软雅黑': 0.95,
        'simsun': 1.0,
        '宋体': 1.0,
        'simhei': 0.95,
        '黑体': 0.95,
        'sans-serif': 0.52,
        'serif': 0.45,
        'monospace': 0.60
    };

    var factor = fontWidthFactors[fontFamily] || 0.55;

    // 计算宽度，考虑中英文字符差异
    var width = 0;
    for (var i = 0; i < text.length; i++) {
        var charCode = text.charCodeAt(i);
        if (charCode > 255) {
            // 中文等宽字符
            width += fontSize * 1.0;
        } else {
            // 英文字符
            width += fontSize * factor;
        }
    }

    // 添加微小随机偏移（模拟子像素渲染）
    var seed = 0;
    for (var i = 0; i < text.length && i < 10; i++) {
        seed += text.charCodeAt(i);
    }
    width += (seed % 100) / 1000;

    return {
        width: width,
        actualBoundingBoxAscent: fontSize * 0.8,
        actualBoundingBoxDescent: fontSize * 0.2,
        actualBoundingBoxLeft: 0,
        actualBoundingBoxRight: width,
        fontBoundingBoxAscent: fontSize * 0.85,
        fontBoundingBoxDescent: fontSize * 0.25
    };
};
CanvasRenderingContext2D.prototype.beginPath = function() { this._record('beginPath'); };
CanvasRenderingContext2D.prototype.closePath = function() { this._record('closePath'); };
CanvasRenderingContext2D.prototype.moveTo = function(x, y) { this._record('moveTo:' + x + ',' + y); };
CanvasRenderingContext2D.prototype.lineTo = function(x, y) { this._record('lineTo:' + x + ',' + y); };
CanvasRenderingContext2D.prototype.arc = function(x, y, r, s, e) { this._record('arc:' + x + ',' + y + ',' + r); };
CanvasRenderingContext2D.prototype.arcTo = function(x1, y1, x2, y2, r) { this._record('arcTo'); };
CanvasRenderingContext2D.prototype.rect = function(x, y, w, h) { this._record('rect:' + x + ',' + y + ',' + w + ',' + h); };
CanvasRenderingContext2D.prototype.fill = function() { this._record('fill'); };
CanvasRenderingContext2D.prototype.stroke = function() { this._record('stroke'); };
CanvasRenderingContext2D.prototype.clip = function() {};
CanvasRenderingContext2D.prototype.save = function() {};
CanvasRenderingContext2D.prototype.restore = function() {};
CanvasRenderingContext2D.prototype.translate = function() {};
CanvasRenderingContext2D.prototype.rotate = function() {};
CanvasRenderingContext2D.prototype.scale = function() {};
CanvasRenderingContext2D.prototype.transform = function() {};
CanvasRenderingContext2D.prototype.setTransform = function() {};
CanvasRenderingContext2D.prototype.drawImage = function() { this._record('drawImage'); };
CanvasRenderingContext2D.prototype.createLinearGradient = function() {
    return { addColorStop: function() {} };
};
CanvasRenderingContext2D.prototype.createRadialGradient = function() {
    return { addColorStop: function() {} };
};
CanvasRenderingContext2D.prototype.createPattern = function() { return null; };
CanvasRenderingContext2D.prototype.getImageData = function(x, y, w, h) {
    // 生成伪随机但一致的图像数据
    var data = new Uint8ClampedArray(w * h * 4);
    var seed = 12345;
    for (var i = 0; i < data.length; i += 4) {
        seed = (seed * 1103515245 + 12345) & 0x7fffffff;
        data[i] = seed % 256;     // R
        data[i+1] = (seed >> 8) % 256;   // G
        data[i+2] = (seed >> 16) % 256;  // B
        data[i+3] = 255;          // A
    }
    return { data: data, width: w, height: h };
};
CanvasRenderingContext2D.prototype.putImageData = function() {};
CanvasRenderingContext2D.prototype.createImageData = function(w, h) {
    return { data: new Uint8ClampedArray(w * h * 4), width: w, height: h };
};

function WebGLRenderingContext() {
    this.canvas = null;
    this.drawingBufferWidth = 300;
    this.drawingBufferHeight = 150;
    // WebGL 常量
    this.VERTEX_SHADER = 35633;
    this.FRAGMENT_SHADER = 35632;
    this.ARRAY_BUFFER = 34962;
    this.ELEMENT_ARRAY_BUFFER = 34963;
    this.STATIC_DRAW = 35044;
    this.FLOAT = 5126;
    this.TRIANGLES = 4;
    this.COLOR_BUFFER_BIT = 16384;
    this.DEPTH_BUFFER_BIT = 256;
}
WebGLRenderingContext.prototype.getParameter = function(pname) {
    var params = {
        37445: 'Intel Inc.',  // UNMASKED_VENDOR_WEBGL
        37446: 'Intel(R) UHD Graphics 630',  // UNMASKED_RENDERER_WEBGL - 更真实的值
        7936: 'WebKit',  // VENDOR
        7937: 'WebKit WebGL',  // RENDERER
        7938: 'WebGL 1.0 (OpenGL ES 2.0 Chromium)',  // VERSION
        7939: 'WebGL GLSL ES 1.0 (OpenGL ES GLSL ES 1.0 Chromium)',  // SHADING_LANGUAGE_VERSION
        3379: 16384,  // MAX_TEXTURE_SIZE
        34076: 16384,  // MAX_CUBE_MAP_TEXTURE_SIZE
        34024: 16384,  // MAX_RENDERBUFFER_SIZE
        35661: 80,  // MAX_COMBINED_TEXTURE_IMAGE_UNITS
        34930: 16,  // MAX_TEXTURE_IMAGE_UNITS
        35660: 16,  // MAX_VERTEX_TEXTURE_IMAGE_UNITS
        36347: 4096,  // MAX_VERTEX_UNIFORM_VECTORS
        36348: 1024,  // MAX_FRAGMENT_UNIFORM_VECTORS
        36349: 30,  // MAX_VARYING_VECTORS
        34921: 16,  // MAX_VERTEX_ATTRIBS
        3386: new Int32Array([32767, 32767]),  // MAX_VIEWPORT_DIMS
        32883: 256,  // MAX_3D_TEXTURE_SIZE
        35071: 16,  // MAX_ARRAY_TEXTURE_LAYERS
        36203: 8,  // MAX_COLOR_ATTACHMENTS
        35968: 8,  // MAX_DRAW_BUFFERS
        33001: 16,  // MAX_ELEMENTS_INDICES
        33000: 1048576,  // MAX_ELEMENTS_VERTICES
        35373: 4096,  // MAX_FRAGMENT_INPUT_COMPONENTS
        35657: 16,  // MAX_FRAGMENT_UNIFORM_BLOCKS
        35658: 65536,  // MAX_FRAGMENT_UNIFORM_COMPONENTS
        35377: 65536,  // MAX_PROGRAM_TEXEL_OFFSET
        36183: 4,  // MAX_SAMPLES
        35659: 12,  // MAX_UNIFORM_BUFFER_BINDINGS
        35376: -8,  // MIN_PROGRAM_TEXEL_OFFSET
        7937: 'WebKit WebGL',  // RENDERER
        2849: 1,  // LINE_WIDTH
        32824: new Float32Array([1, 1]),  // ALIASED_LINE_WIDTH_RANGE
        33902: new Float32Array([1, 1024]),  // ALIASED_POINT_SIZE_RANGE
    };
    return params[pname] !== undefined ? params[pname] : null;
};
WebGLRenderingContext.prototype.getExtension = function(name) {
    var extensions = {
        'WEBGL_debug_renderer_info': { UNMASKED_VENDOR_WEBGL: 37445, UNMASKED_RENDERER_WEBGL: 37446 },
        'EXT_texture_filter_anisotropic': { MAX_TEXTURE_MAX_ANISOTROPY_EXT: 34047, TEXTURE_MAX_ANISOTROPY_EXT: 34046 },
        'WEBKIT_EXT_texture_filter_anisotropic': { MAX_TEXTURE_MAX_ANISOTROPY_EXT: 34047, TEXTURE_MAX_ANISOTROPY_EXT: 34046 },
        'OES_texture_float': {},
        'OES_texture_float_linear': {},
        'OES_texture_half_float': { HALF_FLOAT_OES: 36193 },
        'OES_texture_half_float_linear': {},
        'OES_standard_derivatives': { FRAGMENT_SHADER_DERIVATIVE_HINT_OES: 35723 },
        'OES_element_index_uint': {},
        'OES_vertex_array_object': {},
        'WEBGL_lose_context': { loseContext: function() {}, restoreContext: function() {} },
        'WEBGL_compressed_texture_s3tc': {},
        'WEBGL_depth_texture': {},
        'EXT_blend_minmax': {},
        'EXT_frag_depth': {},
        'EXT_shader_texture_lod': {},
        'EXT_sRGB': {},
        'WEBGL_draw_buffers': {}
    };
    return extensions[name] || null;
};
WebGLRenderingContext.prototype.getSupportedExtensions = function() {
    return [
        'ANGLE_instanced_arrays',
        'EXT_blend_minmax',
        'EXT_color_buffer_half_float',
        'EXT_disjoint_timer_query',
        'EXT_float_blend',
        'EXT_frag_depth',
        'EXT_shader_texture_lod',
        'EXT_texture_compression_rgtc',
        'EXT_texture_filter_anisotropic',
        'EXT_sRGB',
        'OES_element_index_uint',
        'OES_fbo_render_mipmap',
        'OES_standard_derivatives',
        'OES_texture_float',
        'OES_texture_float_linear',
        'OES_texture_half_float',
        'OES_texture_half_float_linear',
        'OES_vertex_array_object',
        'WEBGL_color_buffer_float',
        'WEBGL_compressed_texture_s3tc',
        'WEBGL_compressed_texture_s3tc_srgb',
        'WEBGL_debug_renderer_info',
        'WEBGL_debug_shaders',
        'WEBGL_depth_texture',
        'WEBGL_draw_buffers',
        'WEBGL_lose_context',
        'WEBGL_multi_draw'
    ];
};
WebGLRenderingContext.prototype.createShader = function() { return {}; };
WebGLRenderingContext.prototype.shaderSource = function() {};
WebGLRenderingContext.prototype.compileShader = function() {};
WebGLRenderingContext.prototype.getShaderParameter = function() { return true; };
WebGLRenderingContext.prototype.createProgram = function() { return {}; };
WebGLRenderingContext.prototype.attachShader = function() {};
WebGLRenderingContext.prototype.linkProgram = function() {};
WebGLRenderingContext.prototype.getProgramParameter = function() { return true; };
WebGLRenderingContext.prototype.useProgram = function() {};
WebGLRenderingContext.prototype.getShaderInfoLog = function() { return ''; };
WebGLRenderingContext.prototype.getProgramInfoLog = function() { return ''; };
WebGLRenderingContext.prototype.deleteShader = function() {};
WebGLRenderingContext.prototype.deleteProgram = function() {};
WebGLRenderingContext.prototype.getContextAttributes = function() {
    return { alpha: true, antialias: true, depth: true, stencil: false };
};

function HTMLCanvasElement() {
    this.width = 300;
    this.height = 150;
    this._context2d = new CanvasRenderingContext2D();
    this._context2d.canvas = this;
    // 生成基于 cookie/uifid 的伪随机种子，确保指纹一致性
    this._fingerprintSeed = 0x12345678;
}
HTMLCanvasElement.prototype.getContext = function(type) {
    if (type === '2d') {
        return this._context2d;
    } else if (type === 'webgl' || type === 'experimental-webgl') {
        return new WebGLRenderingContext();
    }
    return null;
};
HTMLCanvasElement.prototype.toDataURL = function(type) {
    // 生成动态但一致的 Canvas 指纹
    // 模拟真实浏览器的 Canvas 渲染差异
    var seed = this._fingerprintSeed;
    var hash = function(str) {
        var h = seed;
        for (var i = 0; i < str.length; i++) {
            h = ((h << 5) - h) + str.charCodeAt(i);
            h = h & h;
        }
        return h;
    };

    // 基于画布内容生成伪随机但稳定的 base64
    var content = (this._context2d._drawHistory || []).join('|');
    var fingerprint = hash(content + this.width + this.height);

    // 生成看起来像真实 PNG 的 base64（实际是固定模板 + 微小变化）
    var templates = [
        'iVBORw0KGgoAAAANSUhEUgAAASwAAACWCAYAAABkW7XSAAA',
        'iVBORw0KGgoAAAANSUhEUgAABLAAAAGwCAYAAACWCAYAAA',
        'iVBORw0KGgoAAAANSUhEUgAAAMgAAADICAYAAACtWK6eAAA'
    ];
    var base = templates[Math.abs(fingerprint) % templates.length];
    // 添加伪随机后缀
    var suffix = Math.abs(fingerprint).toString(36).substring(0, 20);
    return 'data:image/png;base64,' + base + suffix + 'BJRU5ErkJggg==';
};
HTMLCanvasElement.prototype.toBlob = function(callback, type, quality) {
    var dataUrl = this.toDataURL(type);
    var base64 = dataUrl.split(',')[1];
    // 模拟异步回调
    var self = this;
    setTimeout(function() {
        callback(new Blob([base64], { type: type || 'image/png' }));
    }, 0);
};

// 模拟 URL
var _URL = {
    createObjectURL: function(blob) { return 'blob:' + Math.random().toString(36); },
    revokeObjectURL: function(url) {}
};

// 模拟 self 和 globalThis
var self = window;
if (typeof globalThis === 'undefined') {
    var globalThis = global;
}
globalThis.window = window;
globalThis.document = document;
globalThis.navigator = navigator;
globalThis.location = location;
globalThis.screen = screen;
globalThis.self = self;
globalThis.windows = windows;
globalThis.XMLHttpRequest = XMLHttpRequest;
globalThis.Image = Image;
globalThis.Audio = Audio;
globalThis.MutationObserver = MutationObserver;
globalThis.WebKitMutationObserver = MutationObserver;
globalThis.ResizeObserver = ResizeObserver;
globalThis.IntersectionObserver = IntersectionObserver;
globalThis.PerformanceObserver = PerformanceObserver;
globalThis.WebSocket = WebSocket;
globalThis.Worker = Worker;
globalThis.Blob = Blob;
globalThis.File = File;
globalThis.FileReader = FileReader;
globalThis.CanvasRenderingContext2D = CanvasRenderingContext2D;
globalThis.WebGLRenderingContext = WebGLRenderingContext;
globalThis.HTMLCanvasElement = HTMLCanvasElement;
globalThis.requestAnimationFrame = window.requestAnimationFrame;
globalThis.cancelAnimationFrame = window.cancelAnimationFrame;
// AudioContext（重要！用于音频指纹）
globalThis.AudioContext = AudioContext;
globalThis.webkitAudioContext = AudioContext;
globalThis.OfflineAudioContext = OfflineAudioContext;
globalThis.webkitOfflineAudioContext = OfflineAudioContext;
// Chrome 对象（重要！用于 Chrome 浏览器检测）
globalThis.chrome = window.chrome;

// 导出鼠标轨迹数据到全局（bdms 可能会读取）
globalThis._mouseTrackData = _mouseTrackData;
globalThis._bd_data = _mouseTrackData;
globalThis.__bdms_data = _mouseTrackData;

// 重写全局 fetch，让 bdms.js 能够拦截它
globalThis.fetch = window.fetch;
globalThis.URL = globalThis.URL || _URL;

// 模拟 Headers 类（让 bdms 可以重写 fetch）
function Headers(init) {
    this._headers = {};
    if (init) {
        if (init instanceof Headers) {
            this._headers = Object.assign({}, init._headers);
        } else if (typeof init === 'object') {
            for (var key in init) {
                if (init.hasOwnProperty(key)) {
                    this._headers[key.toLowerCase()] = init[key];
                }
            }
        }
    }
}
Headers.prototype.append = function(name, value) {
    this._headers[name.toLowerCase()] = value;
};
Headers.prototype.delete = function(name) {
    delete this._headers[name.toLowerCase()];
};
Headers.prototype.get = function(name) {
    return this._headers[name.toLowerCase()] || null;
};
Headers.prototype.has = function(name) {
    return name.toLowerCase() in this._headers;
};
Headers.prototype.set = function(name, value) {
    this._headers[name.toLowerCase()] = value;
};
Headers.prototype.forEach = function(callback) {
    for (var key in this._headers) {
        callback(this._headers[key], key, this);
    }
};
globalThis.Headers = Headers;

// 模拟 Request 类
function Request(input, init) {
    this.url = typeof input === 'string' ? input : input.url;
    this.method = (init && init.method) || 'GET';
    this.headers = new Headers((init && init.headers) || {});
    this.body = (init && init.body) || null;
}
globalThis.Request = Request;

globalThis.atob = function(str) {
    return Buffer.from(str, 'base64').toString('binary');
};
globalThis.btoa = function(str) {
    return Buffer.from(str, 'binary').toString('base64');
};

// 模拟 queueMicrotask
if (typeof queueMicrotask === 'undefined') {
    var queueMicrotask = function(callback) {
        Promise.resolve().then(callback);
    };
}

// 模拟 TextDecoder 和 TextEncoder
if (typeof TextDecoder === 'undefined') {
    var TextDecoder = require('util').TextDecoder;
}
if (typeof TextEncoder === 'undefined') {
    var TextEncoder = require('util').TextEncoder;
}

// 钩住 URLSearchParams.prototype.set 来捕获 a_bogus
const originalURLSearchParamsSet = URLSearchParams.prototype.set;
URLSearchParams.prototype.set = function(key, value) {
    if (key === 'a_bogus') {
        windows.a_bogus = value;
        // console.log('[Hook] a_bogus captured:', value);
    }
    return originalURLSearchParamsSet.call(this, key, value);
};

// 导出环境变量
module.exports = {
    window: window,
    document: document,
    navigator: navigator,
    location: location,
    screen: screen,
    self: self,
    globalThis: globalThis,
    windows: windows,

    // 更新环境配置的方法
    updateLocation: function(url) {
        try {
            var urlObj = new URL(url);
            window.location.href = url;
            window.location.pathname = urlObj.pathname;
            window.location.search = urlObj.search;
            window.location.host = urlObj.host;
            window.location.hostname = urlObj.hostname;
            window.location.protocol = urlObj.protocol;
            window.location.origin = urlObj.origin;
            document.URL = url;
        } catch (e) {
            console.error("Invalid URL:", url);
        }
    },

    updateUserAgent: function(ua) {
        window.navigator.userAgent = ua;
        navigator.userAgent = ua;
    },

    updateScreen: function(width, height) {
        window.screen.width = width;
        window.screen.height = height;
        screen.width = width;
        screen.height = height;
    },

    // 获取 a_bogus 值
    getABogus: function() {
        return windows.a_bogus;
    },

    // 重置 a_bogus 值
    resetABogus: function() {
        windows.a_bogus = null;
    },

    // 触发鼠标移动事件（bdms 需要至少一次鼠标事件才能生成有效签名）
    triggerMouseEvent: function() {
        var x = Math.floor(Math.random() * 1000);
        var y = Math.floor(Math.random() * 800);
        var movementX = Math.floor(Math.random() * 10);
        var movementY = Math.floor(Math.random() * 10);
        var now = Date.now();

        var mouseEvent = {
            type: 'mousemove',
            clientX: x,
            clientY: y,
            screenX: x,
            screenY: y + 100,
            pageX: x,
            pageY: y,
            movementX: movementX,
            movementY: movementY,
            target: document.body || document,
            currentTarget: document,
            bubbles: true,
            cancelable: true,
            timeStamp: now,
            isTrusted: true
        };

        // 更新鼠标轨迹数据存储（bdms 会读取这些数据）
        var dx = x - _mouseTrackData.lastX;
        var dy = y - _mouseTrackData.lastY;
        var distance = Math.sqrt(dx * dx + dy * dy);

        _mouseTrackData.movements.push({
            x: x,
            y: y,
            t: now - _mouseTrackData.startTime,
            ts: now
        });
        _mouseTrackData.lastX = x;
        _mouseTrackData.lastY = y;
        _mouseTrackData.totalDistance += distance;
        _mouseTrackData.moveCount++;

        // 限制存储的轨迹点数量，避免内存溢出
        if (_mouseTrackData.movements.length > 100) {
            _mouseTrackData.movements = _mouseTrackData.movements.slice(-50);
        }

        // 触发 window 上的事件
        if (window._eventListeners && window._eventListeners['mousemove']) {
            for (var i = 0; i < window._eventListeners['mousemove'].length; i++) {
                try {
                    window._eventListeners['mousemove'][i].call(window, mouseEvent);
                } catch(e) {}
            }
        }
        // 触发 document 上的事件
        if (document._eventListeners && document._eventListeners['mousemove']) {
            for (var i = 0; i < document._eventListeners['mousemove'].length; i++) {
                try {
                    document._eventListeners['mousemove'][i].call(document, mouseEvent);
                } catch(e) {}
            }
        }
    },

    // 模拟鼠标轨迹（生成一系列连续的鼠标移动事件，模拟真实用户行为）
    simulateMouseTrack: function(options) {
        options = options || {};
        var points = options.points || 20;  // 轨迹点数量
        var startX = options.startX || Math.floor(Math.random() * 500);
        var startY = options.startY || Math.floor(Math.random() * 400);
        var endX = options.endX || Math.floor(Math.random() * 500 + 500);
        var endY = options.endY || Math.floor(Math.random() * 400 + 200);
        var duration = options.duration || 500;  // 总时长（毫秒）

        var track = [];
        var baseTime = Date.now();

        // 使用贝塞尔曲线生成自然的鼠标轨迹
        // 控制点添加一些随机偏移，使轨迹更自然
        var ctrlX1 = startX + (endX - startX) * 0.25 + (Math.random() - 0.5) * 100;
        var ctrlY1 = startY + (endY - startY) * 0.25 + (Math.random() - 0.5) * 80;
        var ctrlX2 = startX + (endX - startX) * 0.75 + (Math.random() - 0.5) * 100;
        var ctrlY2 = startY + (endY - startY) * 0.75 + (Math.random() - 0.5) * 80;

        // 贝塞尔曲线计算函数
        function bezier(t, p0, p1, p2, p3) {
            var u = 1 - t;
            return u * u * u * p0 + 3 * u * u * t * p1 + 3 * u * t * t * p2 + t * t * t * p3;
        }

        // 生成轨迹点
        var prevX = startX;
        var prevY = startY;

        for (var i = 0; i < points; i++) {
            // 使用非线性时间分布，模拟加速-减速的自然移动
            var t = i / (points - 1);
            var easeT = t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;  // easeInOutQuad

            var x = Math.floor(bezier(easeT, startX, ctrlX1, ctrlX2, endX));
            var y = Math.floor(bezier(easeT, startY, ctrlY1, ctrlY2, endY));

            // 添加微小的随机抖动，模拟手部自然抖动
            x += Math.floor((Math.random() - 0.5) * 3);
            y += Math.floor((Math.random() - 0.5) * 3);

            var movementX = x - prevX;
            var movementY = y - prevY;

            // 时间间隔也添加一些随机性
            var timeOffset = Math.floor(duration * t + (Math.random() - 0.5) * 20);
            var eventTime = baseTime + timeOffset;

            // 计算移动距离并更新轨迹数据存储
            var dx = x - prevX;
            var dy = y - prevY;
            var distance = Math.sqrt(dx * dx + dy * dy);

            _mouseTrackData.movements.push({
                x: x,
                y: y,
                t: eventTime - _mouseTrackData.startTime,
                ts: eventTime
            });
            _mouseTrackData.totalDistance += distance;
            _mouseTrackData.moveCount++;

            track.push({
                type: 'mousemove',
                clientX: x,
                clientY: y,
                screenX: x,
                screenY: y + 100,  // 屏幕坐标通常比客户区坐标大（标题栏等）
                pageX: x,
                pageY: y,
                movementX: movementX,
                movementY: movementY,
                target: document.body || document,
                currentTarget: document,
                bubbles: true,
                cancelable: true,
                timeStamp: eventTime,
                isTrusted: true
            });

            prevX = x;
            prevY = y;
        }

        // 更新最后位置
        _mouseTrackData.lastX = prevX;
        _mouseTrackData.lastY = prevY;

        // 限制存储的轨迹点数量
        if (_mouseTrackData.movements.length > 100) {
            _mouseTrackData.movements = _mouseTrackData.movements.slice(-50);
        }

        // 触发所有轨迹点事件
        var self = this;
        track.forEach(function(event) {
            // 触发 window 上的事件
            if (window._eventListeners && window._eventListeners['mousemove']) {
                for (var i = 0; i < window._eventListeners['mousemove'].length; i++) {
                    try {
                        window._eventListeners['mousemove'][i].call(window, event);
                    } catch(e) {}
                }
            }
            // 触发 document 上的事件
            if (document._eventListeners && document._eventListeners['mousemove']) {
                for (var i = 0; i < document._eventListeners['mousemove'].length; i++) {
                    try {
                        document._eventListeners['mousemove'][i].call(document, event);
                    } catch(e) {}
                }
            }
        });

        return track;
    },

    // 获取鼠标轨迹数据（供调试使用）
    getMouseTrackData: function() {
        return _mouseTrackData;
    },

    // 重置鼠标轨迹数据
    resetMouseTrackData: function() {
        _mouseTrackData.movements = [];
        _mouseTrackData.lastX = 0;
        _mouseTrackData.lastY = 0;
        _mouseTrackData.startTime = Date.now();
        _mouseTrackData.totalDistance = 0;
        _mouseTrackData.moveCount = 0;
    },

    // 恢复 process 对象（供需要时使用）
    restoreProcess: function() {
        if (global._process) {
            global.process = global._process;
        }
    }
};
