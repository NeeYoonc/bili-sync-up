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
		func: () => {
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

async function transfer(platform) {
	setBusy(true);
	const douyin = platform === 'douyin';
	const label = douyin ? '抖音' : 'YouTube';
	showStatus(`正在读取并验证${label}登录状态…`);
	try {
		const serverUrl = normalizeServerUrl(serverInput.value);
		const authToken = tokenInput.value.trim();
		if (!authToken) throw new Error('请先连接 Bili Sync 设置页或填写 API Token');

		const granted = await chrome.permissions.request({
			origins: [permissionPattern(serverUrl)]
		});
		if (!granted) throw new Error('未授权连接 Bili Sync 地址，无法传输登录状态');

		const cookies = douyin
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
		const response = await fetch(`${serverUrl}/api/${platform}/cookies`, {
			method: 'POST',
			headers: {
				Authorization: authToken,
				'Content-Type': 'application/json'
			},
			body: JSON.stringify({ cookies: contents, ...douyinSessionParams })
		});
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

loadConfig().catch((error) => {
	showStatus(error instanceof Error ? error.message : String(error), 'error');
});
