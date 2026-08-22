import {
	buildNetscapeCookies,
	normalizeServerUrl,
	permissionPattern,
	responseMessage
} from './helper.js';

const serverInput = document.querySelector('#server-url');
const tokenInput = document.querySelector('#auth-token');
const connectButton = document.querySelector('#connect-page');
const openButton = document.querySelector('#open-youtube');
const transferButton = document.querySelector('#transfer');
const openDouyinButton = document.querySelector('#open-douyin');
const transferDouyinButton = document.querySelector('#transfer-douyin');
const openTikTokButton = document.querySelector('#open-tiktok');
const transferTikTokButton = document.querySelector('#transfer-tiktok');
const statusBox = document.querySelector('#status');

function showStatus(message, kind = '') {
	statusBox.textContent = message;
	statusBox.className = kind;
}

function setBusy(busy) {
	connectButton.disabled = busy;
	openButton.disabled = busy;
	transferButton.disabled = busy;
	openDouyinButton.disabled = busy;
	transferDouyinButton.disabled = busy;
	openTikTokButton.disabled = busy;
	transferTikTokButton.disabled = busy;
}

async function saveConfig() {
	await chrome.storage.local.set({
		serverUrl: serverInput.value.trim(),
		authToken: tokenInput.value
	});
}

async function loadConfig() {
	const config = await chrome.storage.local.get(['serverUrl', 'authToken']);
	serverInput.value = config.serverUrl || '';
	tokenInput.value = config.authToken || '';
}

async function captureDouyinSessionParams() {
	const tabs = await chrome.tabs.query({
		url: ['https://douyin.com/*', 'https://*.douyin.com/*']
	});
	const tab = tabs
		.filter((item) => item.id)
		.sort((left, right) => (right.lastAccessed || 0) - (left.lastAccessed || 0))[0];
	if (!tab?.id) return {};

	const [{ result }] = await chrome.scripting.executeScript({
		target: { tabId: tab.id },
		world: 'MAIN',
		awaitPromise: true,
		func: async () => {
			const result = {};
			const urls = [
				...performance
					.getEntriesByType('resource')
					.map((entry) => entry.name)
					.reverse(),
				window.location.href
			];
			for (const value of urls) {
				try {
					const url = new URL(value);
					result.webid ||= url.searchParams.get('webid') || '';
					result.verify_fp ||=
						url.searchParams.get('verifyFp') || url.searchParams.get('fp') || '';
					result.ms_token ||= url.searchParams.get('msToken') || '';
					if (result.webid && result.verify_fp && result.ms_token) break;
				} catch {
					// 忽略无效或非 HTTP 资源地址。
				}
			}
			// 收集抖音 secsdk 会话密钥（localStorage + IndexedDB 的 security-sdk/SLARDAR 等）。
			// 「我的喜欢」「收藏夹」接口签名需要这些密钥，服务端会写入 douyin-secsdk.json。
			const secsdkPrefix = /^(security-sdk\/|SLARDAR|web_runtime_security_uid|web_secsdk_runtime_cache|SysInfo|g_ven)/;
			const local_storage = {};
			for (let i = 0; i < localStorage.length; i++) {
				const key = localStorage.key(i);
				if (key && secsdkPrefix.test(key)) {
					try {
						local_storage[key] = localStorage.getItem(key);
					} catch {
						// 单个键读取失败不影响其余密钥
					}
				}
			}
			// 抖音 secsdk 的部分密钥（s_sdk_crypt_sdk / s_sdk_server_cert_key 等）
			// 存在 IndexedDB 的 secure-store 库，需要一并读取并合并进 local_storage。
			try {
				const openRequest = indexedDB.open('secure-store');
				const db = await new Promise((resolve, reject) => {
					openRequest.onsuccess = () => resolve(openRequest.result);
					openRequest.onerror = () => reject(openRequest.error);
				});
				if (db.objectStoreNames.contains('cryptvalues')) {
					const store = db.transaction('cryptvalues', 'readonly').objectStore('cryptvalues');
					const keys = await new Promise((resolve, reject) => {
						const request = store.getAllKeys();
						request.onsuccess = () => resolve(request.result);
						request.onerror = () => reject(request.error);
					});
					const values = await new Promise((resolve, reject) => {
						const request = store.getAll();
						request.onsuccess = () => resolve(request.result);
						request.onerror = () => reject(request.error);
					});
					(keys || []).forEach((key, index) => {
						if (key && values && values[index] !== undefined && secsdkPrefix.test(String(key))) {
							local_storage[String(key)] = String(values[index]);
						}
					});
				}
			} catch {
				// IndexedDB 读取失败时至少保留 localStorage 中的密钥
			}
			if (Object.keys(local_storage).length > 0) {
				result.local_storage = local_storage;
			}
			result.ua = navigator.userAgent || '';
			result.href = window.location.href || '';
			return result;
		}
	});
	return result || {};
}

connectButton.addEventListener('click', async () => {
	setBusy(true);
	try {
		const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
		if (!tab?.id) throw new Error('没有找到当前 Bili Sync 页面');
		const [{ result }] = await chrome.scripting.executeScript({
			target: { tabId: tab.id },
			func: () => ({
				serverUrl: window.location.origin,
				authToken: window.localStorage.getItem('auth_token') || ''
			})
		});
		if (!result?.authToken) {
			throw new Error('当前页面没有 Bili Sync API Token，请先在 Web 页面登录');
		}
		serverInput.value = normalizeServerUrl(result.serverUrl);
		tokenInput.value = result.authToken;
		await saveConfig();
		showStatus('已连接当前 Bili Sync 页面，可以打开 YouTube 登录。', 'success');
	} catch (error) {
		showStatus(error instanceof Error ? error.message : String(error), 'error');
	} finally {
		setBusy(false);
	}
});

openButton.addEventListener('click', async () => {
	await chrome.tabs.create({ url: 'https://www.youtube.com/' });
});

openDouyinButton.addEventListener('click', async () => {
	await chrome.tabs.create({ url: 'https://www.douyin.com/' });
});

openTikTokButton.addEventListener('click', async () => {
	await chrome.tabs.create({ url: 'https://www.tiktok.com/' });
});

async function transfer(platform) {
	setBusy(true);
	const douyin = platform === 'douyin';
	const tiktok = platform === 'tiktok';
	const label = tiktok ? 'TikTok' : douyin ? '抖音' : 'YouTube';
	showStatus(`正在读取并验证${label}登录状态…`);
	try {
		const serverUrl = normalizeServerUrl(serverInput.value);
		const authToken = tokenInput.value.trim();
		if (!authToken) throw new Error('请先连接 Bili Sync 设置页或填写 API Token');

		// 请求连接权限；服务端已支持 CORS，未授权时跨域请求也能成功，因此不强制阻断
		try {
			await chrome.permissions.request({
				origins: [permissionPattern(serverUrl)]
			});
		} catch {
			// 忽略权限请求异常，继续尝试跨域传输
		}

		const cookies = tiktok
			? [
					...(await chrome.cookies.getAll({ domain: 'tiktok.com' })),
					...(await chrome.cookies.getAll({ domain: 'tiktokcdn.com' }))
				]
			: douyin
				? [
						...(await chrome.cookies.getAll({ domain: 'douyin.com' })),
						...(await chrome.cookies.getAll({ domain: 'bytedance.com' }))
					]
				: [
						...(await chrome.cookies.getAll({ domain: 'youtube.com' })),
						...(await chrome.cookies.getAll({ domain: 'google.com' }))
					];
		const contents = buildNetscapeCookies(cookies, platform);
		const douyinSessionParams = douyin ? await captureDouyinSessionParams() : {};
		const controller = new AbortController();
		const timer = setTimeout(() => controller.abort(), 20000);
		let response;
		try {
			response = await fetch(`${serverUrl}/api/${platform}/cookies`, {
				method: 'POST',
				headers: {
					Authorization: authToken,
					'Content-Type': 'application/json'
				},
				body: JSON.stringify({ cookies: contents, ...douyinSessionParams }),
				signal: controller.signal
			});
		} catch (error) {
			if (error && error.name === 'AbortError') {
				throw new Error('传输超时（20 秒），请确认 Bili Sync 服务已启动后重试');
			}
			throw error;
		} finally {
			clearTimeout(timer);
		}
		const payload = await response.json().catch(() => null);
		if (!response.ok) {
			throw new Error(responseMessage(payload, `Bili Sync 返回 HTTP ${response.status}`));
		}

		serverInput.value = serverUrl;
		await saveConfig();
		showStatus(responseMessage(payload, '登录状态已传输到 Bili Sync'), 'success');
	} catch (error) {
		showStatus(error instanceof Error ? error.message : String(error), 'error');
	} finally {
		setBusy(false);
	}
}

transferButton.addEventListener('click', async () => {
	await transfer('youtube');
});

transferDouyinButton.addEventListener('click', async () => {
	await transfer('douyin');
});

transferTikTokButton.addEventListener('click', async () => {
	await transfer('tiktok');
});

loadConfig().catch((error) => {
	showStatus(error instanceof Error ? error.message : String(error), 'error');
});
